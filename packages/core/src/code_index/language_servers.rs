use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};

use super::{
    IndexError, Language, ProjectStatus, Query, QueryResult, ServerCapabilities, ServerCommand,
    ServerState,
    lsp::{ClientState, LanguageServerSession},
};
use crate::tools::{Cancel, CancelHandle};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ProjectKey {
    pub workspace: PathBuf,
    pub root: PathBuf,
    pub language: Language,
    pub fingerprint: String,
}

struct Running {
    session: LanguageServerSession,
    _permit: Slot,
}
struct Slot {
    permit: Option<OwnedSemaphorePermit>,
    changed: Arc<Notify>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.changed.notify_waiters();
    }
}
struct Entry {
    key: ProjectKey,
    command: ServerCommand,
    capabilities: Mutex<Option<ServerCapabilities>>,
    client: Arc<ClientState>,
    generation: AtomicU64,
    state: Mutex<ServerState>,
    running: AsyncMutex<Option<Running>>,
    owner: CancelHandle,
    cancel: Cancel,
    users: AtomicUsize,
    queries: Semaphore,
    last_used: Mutex<Instant>,
    sequence: u64,
    closed: AtomicBool,
}

/// The sole owner of the server pool, queue and resource policy.
pub(super) struct LanguageServerManager {
    entries: Mutex<BTreeMap<ProjectKey, Arc<Entry>>>,
    slots: Arc<Semaphore>,
    changed: Arc<Notify>,
    next: AtomicU64,
    idle: Duration,
}

impl LanguageServerManager {
    pub fn new() -> Arc<Self> {
        Self::with_limits(8, Duration::from_secs(120))
    }
    pub(super) fn with_limits(limit: usize, idle: Duration) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(BTreeMap::new()),
            slots: Arc::new(Semaphore::new(limit)),
            changed: Arc::new(Notify::new()),
            next: AtomicU64::new(1),
            idle,
        })
    }

    pub fn register(
        &self,
        workspace: &Path,
        root: &Path,
        language: Language,
        command: ServerCommand,
    ) -> ProjectKey {
        let fingerprint =
            blake3::hash(&serde_json::to_vec(&command).expect("ServerCommand is serializable"))
                .to_hex()
                .to_string();
        let key = ProjectKey {
            workspace: workspace.into(),
            root: root.into(),
            language,
            fingerprint,
        };
        let mut entries = self.entries.lock().unwrap();
        entries.entry(key.clone()).or_insert_with(|| {
            let (owner, cancel) = CancelHandle::new();
            let sequence = self.next.fetch_add(1, Ordering::Relaxed);
            Arc::new(Entry {
                key: key.clone(),
                command,
                capabilities: Mutex::new(None),
                client: Arc::new(ClientState::default()),
                generation: AtomicU64::new(sequence),
                state: Mutex::new(ServerState::Queued),
                running: AsyncMutex::new(None),
                owner,
                cancel,
                users: AtomicUsize::new(0),
                queries: Semaphore::new(32),
                last_used: Mutex::new(Instant::now()),
                sequence,
                closed: AtomicBool::new(false),
            })
        });
        key
    }

    fn entry(&self, key: &ProjectKey) -> Result<Arc<Entry>, IndexError> {
        self.entries
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(IndexError::Closed)
    }

    pub fn start(self: &Arc<Self>, key: ProjectKey) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(entry) = manager.entry(&key) {
                let _ = manager.ensure_started(&entry, &entry.cancel).await;
            }
        });
    }

    async fn permit(&self, entry: &Entry, cancel: &Cancel) -> Result<Slot, IndexError> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if entry.closed.load(Ordering::Acquire) {
                return Err(IndexError::Closed);
            }
            if cancel.is_cancelled() {
                return Err(IndexError::Cancelled);
            }
            let (first, victim) = {
                let entries = self.entries.lock().unwrap();
                let first = entries
                    .values()
                    .filter(|candidate| {
                        *candidate.state.lock().unwrap() == ServerState::Queued
                            && !candidate.closed.load(Ordering::Acquire)
                    })
                    .min_by_key(|candidate| {
                        (
                            candidate.users.load(Ordering::Acquire) == 0,
                            candidate.sequence,
                        )
                    })
                    .map(|candidate| candidate.sequence);
                let victim = entries
                    .values()
                    .filter(|candidate| {
                        candidate.sequence != entry.sequence
                            && candidate.users.load(Ordering::Acquire) == 0
                            && matches!(
                                *candidate.state.lock().unwrap(),
                                ServerState::Ready | ServerState::Failed(_)
                            )
                    })
                    .min_by_key(|candidate| *candidate.last_used.lock().unwrap())
                    .cloned();
                (first, victim)
            };
            if first == Some(entry.sequence) {
                if let Ok(permit) = self.slots.clone().try_acquire_owned() {
                    return Ok(Slot {
                        permit: Some(permit),
                        changed: self.changed.clone(),
                    });
                }
                if let Some(victim) = victim {
                    // try_lock prevents eviction of a session serving an active query.
                    if let Ok(mut running) = victim.running.try_lock()
                        && victim.users.load(Ordering::Acquire) == 0
                        && let Some(mut old) = running.take()
                    {
                        *victim.state.lock().unwrap() = ServerState::Idle;
                        old.session.finish().await;
                        drop(old);
                        self.changed.notify_waiters();
                        continue;
                    }
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(IndexError::Cancelled),
                _ = entry.cancel.cancelled() => return Err(IndexError::Closed),
                _ = notified => {},
            }
        }
    }

    async fn ensure_started(&self, entry: &Arc<Entry>, cancel: &Cancel) -> Result<(), IndexError> {
        let mut running = tokio::select! {
            biased;
            _ = entry.cancel.cancelled() => return Err(IndexError::Closed),
            _ = cancel.cancelled() => return Err(IndexError::Cancelled),
            guard = entry.running.lock() => guard,
        };
        if entry.closed.load(Ordering::Acquire) {
            return Err(IndexError::Closed);
        }
        if running
            .as_ref()
            .is_some_and(|running| running.session.alive())
        {
            return Ok(());
        }
        if let Some(mut old) = running.take() {
            old.session.finish().await;
        }
        *entry.state.lock().unwrap() = ServerState::Queued;
        self.changed.notify_waiters();
        let permit = match self.permit(entry, cancel).await {
            Ok(permit) => permit,
            Err(error) => {
                *entry.state.lock().unwrap() = ServerState::Idle;
                self.changed.notify_waiters();
                return Err(error);
            }
        };
        *entry.state.lock().unwrap() = ServerState::Starting;
        *entry.capabilities.lock().unwrap() = None;
        entry.client.progress.lock().unwrap().clear();
        *entry.client.watches.lock().unwrap() = Default::default();
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        entry.generation.store(generation, Ordering::Release);
        // Initialization belongs to the workspace, not whichever chat first requested it.
        match LanguageServerSession::start(
            &entry.key.root,
            &entry.key.workspace,
            entry.key.language,
            &entry.command,
            generation,
            &entry.cancel,
            entry.client.clone(),
        )
        .await
        {
            Ok(session) if !entry.closed.load(Ordering::Acquire) => {
                *entry.capabilities.lock().unwrap() = Some(session.capabilities());
                *running = Some(Running {
                    session,
                    _permit: permit,
                });
                *entry.last_used.lock().unwrap() = Instant::now();
                *entry.state.lock().unwrap() = ServerState::Ready;
                self.changed.notify_waiters();
                Ok(())
            }
            Ok(mut session) => {
                session.finish().await;
                *entry.state.lock().unwrap() = ServerState::Closed;
                self.changed.notify_waiters();
                Err(IndexError::Closed)
            }
            Err(error) => {
                *entry.state.lock().unwrap() = if entry.closed.load(Ordering::Acquire) {
                    ServerState::Closed
                } else {
                    ServerState::Failed(error.to_string())
                };
                self.changed.notify_waiters();
                Err(error)
            }
        }
    }

    pub async fn query(
        self: &Arc<Self>,
        key: &ProjectKey,
        path: &Path,
        query: &Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let entry = self.entry(key)?;
        let _capacity = entry.queries.try_acquire().map_err(|_| IndexError::Busy)?;
        entry.users.fetch_add(1, Ordering::AcqRel);
        struct User<'a>(&'a Entry, &'a Notify);
        impl Drop for User<'_> {
            fn drop(&mut self) {
                self.0.users.fetch_sub(1, Ordering::AcqRel);
                *self.0.last_used.lock().unwrap() = Instant::now();
                self.1.notify_waiters();
            }
        }
        let _user = User(&entry, &self.changed);
        self.changed.notify_waiters();
        let manager = self.clone();
        let starting = entry.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tauri::async_runtime::spawn(async move {
            let result = manager.ensure_started(&starting, &starting.cancel).await;
            let _ = sender.send(result);
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(IndexError::Cancelled),
            _ = entry.cancel.cancelled() => return Err(IndexError::Closed),
            result = receiver => result.map_err(|_| IndexError::Closed)??,
        }
        let mut running = tokio::select! {
            biased;
            _ = entry.cancel.cancelled() => return Err(IndexError::Closed),
            _ = cancel.cancelled() => return Err(IndexError::Cancelled),
            running = entry.running.lock() => running,
        };
        let Some(instance) = running.as_mut() else {
            return Err(IndexError::NotReady);
        };
        let result = tokio::select! {
            biased;
            _ = entry.cancel.cancelled() => Err(IndexError::Closed),
            result = instance.session.query(path, query, cancel) => result,
        };
        if entry.closed.load(Ordering::Acquire) {
            return Err(IndexError::Closed);
        }
        if !instance.session.alive() {
            let error = instance.session.failure();
            if let Some(mut old) = running.take() {
                old.session.finish().await;
            }
            *entry.state.lock().unwrap() = ServerState::Failed(error.to_string());
            self.changed.notify_waiters();
            return Err(error);
        }
        result
    }

    /// Never queue background work ahead of an interactive user or start/evict a server for it.
    pub async fn background_symbols(
        &self,
        key: &ProjectKey,
        path: &Path,
        cancel: &Cancel,
    ) -> Result<(QueryResult, String), IndexError> {
        let entry = self.entry(key)?;
        if entry.users.load(Ordering::Acquire) != 0 {
            return Err(IndexError::Busy);
        }
        let mut running = entry.running.try_lock().map_err(|_| IndexError::Busy)?;
        let instance = running.as_mut().ok_or(IndexError::NotReady)?;
        if !instance.session.alive() {
            return Err(instance.session.failure());
        }
        let (owner, background) = CancelHandle::new();
        let result = {
            let query = instance.session.background_symbols(path, &background);
            tokio::pin!(query);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => { owner.cancel(); query.await },
                _ = entry.cancel.cancelled() => { owner.cancel(); query.await },
                _ = async { loop {
                    let changed = self.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if entry.users.load(Ordering::Acquire) != 0 { break; }
                    changed.await;
                }} => { owner.cancel(); let _ = query.await; Err(IndexError::Busy) },
                result = &mut query => result,
            }
        };
        *entry.last_used.lock().unwrap() = Instant::now();
        result
    }

    pub fn watches(&self, workspace: &Path, path: &Path) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.key.workspace == workspace)
            .any(|entry| {
                let rules = entry.client.watches.lock().unwrap();
                (1..=3).any(|kind| rules.matches(path, kind))
            })
    }

    pub async fn files_changed(
        &self,
        workspace: &Path,
        changes: &[(PathBuf, u64)],
    ) -> Result<(), IndexError> {
        let entries: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.key.workspace == workspace)
            .cloned()
            .collect();
        for entry in entries {
            let mut running = tokio::select! { _ = entry.cancel.cancelled() => continue, guard = entry.running.lock() => guard };
            if let Some(instance) = running.as_mut() {
                tokio::select! { _ = entry.cancel.cancelled() => {}, result = instance.session.files_changed(changes) => result? }
            }
        }
        Ok(())
    }

    pub fn retire(&self, key: &ProjectKey) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let entry = self.entries.lock().unwrap().remove(key)?;
        entry.closed.store(true, Ordering::Release);
        entry.owner.cancel();
        self.changed.notify_waiters();
        Some(tauri::async_runtime::spawn(async move {
            if let Some(mut old) = entry.running.lock().await.take() {
                old.session.finish().await;
            }
        }))
    }

    pub fn status(&self, key: &ProjectKey) -> Option<ProjectStatus> {
        let entry = self.entry(key).ok()?;
        if let Ok(running) = entry.running.try_lock()
            && let Some(running) = running.as_ref()
            && !running.session.alive()
        {
            *entry.state.lock().unwrap() =
                ServerState::Failed(running.session.failure().to_string());
        }
        let state = entry.state.lock().unwrap().clone();
        Some(ProjectStatus {
            version: Some(entry.command.version.clone()),
            root: key.root.clone(),
            language: key.language,
            state,
            capabilities: entry.capabilities.lock().unwrap().clone(),
            progress: entry
                .client
                .progress
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect(),
            generation: entry.generation.load(Ordering::Acquire),
        })
    }

    pub async fn restart(&self, key: &ProjectKey, cancel: &Cancel) -> Result<(), IndexError> {
        let entry = self.entry(key)?;
        {
            let mut running = tokio::select! { _ = cancel.cancelled() => return Err(IndexError::Cancelled), running = entry.running.lock() => running };
            if let Some(mut old) = running.take() {
                old.session.finish().await;
            }
            *entry.state.lock().unwrap() = ServerState::Idle;
        }
        self.changed.notify_waiters();
        self.ensure_started(&entry, cancel).await
    }

    pub async fn reap_idle(&self) {
        let entries: Vec<_> = self.entries.lock().unwrap().values().cloned().collect();
        for entry in entries {
            if entry.users.load(Ordering::Acquire) != 0 {
                continue;
            }
            if let Ok(mut running) = entry.running.try_lock()
                && (running
                    .as_ref()
                    .is_some_and(|running| !running.session.alive())
                    || (*entry.last_used.lock().unwrap()).elapsed() >= self.idle)
                && let Some(mut old) = running.take()
            {
                *entry.state.lock().unwrap() = ServerState::Idle;
                old.session.finish().await;
                drop(old);
            }
        }
        self.changed.notify_waiters();
    }

    /// Mark closed synchronously so a late startup can never re-register this workspace.
    pub fn close_workspace(
        self: &Arc<Self>,
        workspace: &Path,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let entries = {
            let mut registry = self.entries.lock().unwrap();
            let keys: Vec<_> = registry
                .keys()
                .filter(|key| key.workspace == workspace)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key))
                .collect::<Vec<_>>()
        };
        for entry in &entries {
            entry.closed.store(true, Ordering::Release);
            entry.owner.cancel();
            *entry.state.lock().unwrap() = ServerState::Closed;
        }
        self.changed.notify_waiters();
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            for entry in entries {
                let mut running = entry.running.lock().await;
                if let Some(mut old) = running.take() {
                    old.session.finish().await;
                }
            }
            manager.changed.notify_waiters();
        })
    }
}
