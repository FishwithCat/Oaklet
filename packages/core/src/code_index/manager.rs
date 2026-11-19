use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use super::{
    CodeIndexConfig, Discovery, IndexError, InstallStatus, ProjectStatus, Query, QueryCoverage,
    QueryData, QueryResult, QuerySource, ServerState, WorkspaceStatus, install,
    language_servers::{LanguageServerManager, ProjectKey},
};
use crate::tools::{Cancel, CancelHandle, Workspace};

#[path = "cache.rs"]
mod cache;

#[path = "persistence.rs"]
mod persistence;

struct WorkspaceEntry {
    config_updates: tokio::sync::watch::Sender<CodeIndexConfig>,
    index_worker: Option<tauri::async_runtime::JoinHandle<()>>,
    persistence: Option<super::PersistenceStatus>,
    index_dirty: Option<std::collections::BTreeSet<PathBuf>>,
    generation: u64,
    owner: CancelHandle,
    cancel: Cancel,
    discovery: Option<Discovery>,
    config: CodeIndexConfig,
    projects: Vec<(ProjectStatus, Option<ProjectKey>)>,
    revision: u64,
    stale: bool,
    watch_error: Option<String>,
    worker: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl WorkspaceEntry {
    fn config_pending(&self) -> bool {
        super::Language::ALL.into_iter().any(|language| {
            self.config
                .server_changed(&self.config_updates.borrow(), language)
        })
    }
}

/// Shared Rust service; no protocol or model-specific types cross this boundary.
pub struct CodeIndexManager {
    settings: Mutex<Option<CodeIndexConfig>>,
    snapshots: Mutex<BTreeMap<PathBuf, (Vec<u8>, u64)>>,
    status_revision: AtomicU64,
    cache: Mutex<cache::QueryCache>,
    store: Option<crate::store::Store>,
    installation_root: Option<PathBuf>,
    installs: install::Jobs,
    workspaces: Arc<Mutex<BTreeMap<PathBuf, WorkspaceEntry>>>,
    servers: Arc<LanguageServerManager>,
    next: AtomicU64,
    shutting_down: AtomicBool,
    closing: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
    draining: tokio::sync::Mutex<()>,
}

impl Default for CodeIndexManager {
    fn default() -> Self {
        Self::new()
    }
}
impl CodeIndexManager {
    /// Volatile service for protocol probes; desktop setup injects the shared Store.
    pub fn new() -> Self {
        Self {
            settings: Mutex::new(None),
            snapshots: Mutex::new(BTreeMap::new()),
            status_revision: AtomicU64::new(1),
            cache: Mutex::new(cache::QueryCache::default()),
            store: None,
            installation_root: None,
            installs: Default::default(),
            workspaces: Arc::new(Mutex::new(BTreeMap::new())),
            servers: LanguageServerManager::new(),
            next: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
            closing: Mutex::new(Vec::new()),
            draining: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) fn with_store(store: crate::store::Store) -> Self {
        Self {
            store: Some(store),
            ..Self::new()
        }
    }

    pub(crate) fn with_installation_root(mut self, root: PathBuf) -> Self {
        self.installation_root = Some(root);
        self
    }

    async fn detect_server(
        &self,
        project: &super::Project,
        config: &CodeIndexConfig,
    ) -> Result<Option<super::ServerCommand>, String> {
        let Some(root) = &self.installation_root else {
            return super::detect_server(project, config).await;
        };
        let directory = install::available_release(root, project.language);
        super::runtime::detect(
            project,
            config,
            std::env::var_os("PATH").as_deref(),
            directory
                .as_deref()
                .or((project.language == super::Language::Rust).then_some(root)),
        )
        .await
    }

    pub(super) fn install_server(
        self: &Arc<Self>,
        workspace: &Path,
        project_root: &Path,
        language: super::Language,
    ) -> Result<(), String> {
        let root = self
            .installation_root
            .clone()
            .ok_or("Managed installation is unavailable.")?;
        let workspaces = self.workspaces.lock().unwrap();
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(IndexError::Closed.to_string());
        }
        let entry = workspaces
            .get(workspace)
            .ok_or(IndexError::Closed.to_string())?;
        let project = entry
            .discovery
            .as_ref()
            .and_then(|discovery| {
                discovery
                    .projects
                    .iter()
                    .find(|p| p.root == project_root && p.language == language)
            })
            .ok_or("Unknown code index project.")?
            .clone();
        if let Some(error) = project.error {
            return Err(error);
        }
        if entry
            .config_updates
            .borrow()
            .servers
            .get(&language)
            .is_some_and(|server| !server.enabled || server.command.is_some())
        {
            return Err(
                "Enable this server and clear the custom executable in saved settings first."
                    .into(),
            );
        }

        let mut installs = self.installs.lock().unwrap();
        if installs
            .get(&language)
            .is_some_and(|job| job.status.active())
        {
            return Ok(());
        }
        let (owner, cancel) = CancelHandle::new();
        let manager = self.clone();
        let workspace = workspace.to_path_buf();
        let task = tauri::async_runtime::spawn(async move {
            let path = std::env::var_os("PATH");
            let result = install::install(
                &root,
                &project,
                &workspace,
                path.as_deref(),
                &cancel,
                || {
                    manager.install_status(language, InstallStatus::Verifying);
                },
            )
            .await;
            let status = match result {
                Ok(()) => {
                    manager.install_status(language, InstallStatus::Activating);
                    manager.activate_install(language, &cancel).await;
                    InstallStatus::Installed
                }
                Err(_) if cancel.is_cancelled() => InstallStatus::Cancelled,
                Err(error) => InstallStatus::Failed(error),
            };
            manager.install_status(language, status);
        });
        installs.insert(
            language,
            install::Job {
                status: InstallStatus::Installing,
                owner,
                task,
            },
        );
        Ok(())
    }

    fn install_status(&self, language: super::Language, status: InstallStatus) {
        if let Some(job) = self.installs.lock().unwrap().get_mut(&language) {
            job.status = status;
        }
    }

    pub(super) fn cancel_install(&self, language: super::Language) {
        if let Some(job) = self.installs.lock().unwrap().get(&language)
            && matches!(
                job.status,
                InstallStatus::Installing | InstallStatus::Verifying
            )
        {
            job.owner.cancel();
        }
    }

    async fn activate_install(&self, language: super::Language, cancel: &Cancel) {
        let targets = self
            .workspaces
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(workspace, entry)| {
                entry
                    .projects
                    .iter()
                    .filter(move |(project, _)| project.language == language)
                    .map(move |(project, _)| (workspace.clone(), project.root.clone()))
            })
            .collect::<Vec<_>>();
        for (workspace, root) in targets {
            if cancel.is_cancelled() {
                break;
            }
            let _ = self
                .retry_project(&workspace, &root, language, cancel, true)
                .await;
        }
    }

    /// Register immediately; discovery and initialization never block a window opening.
    pub fn open_workspace(
        self: &Arc<Self>,
        workspace: &Path,
        config: CodeIndexConfig,
    ) -> Result<(), IndexError> {
        let mut config = config;
        config.validate().map_err(IndexError::Invalid)?;
        let workspace = Workspace::new(&workspace.to_string_lossy())
            .map_err(IndexError::Invalid)?
            .root()
            .to_path_buf();
        let (generation, cancel, mut updates, config) = {
            let mut entries = self.workspaces.lock().unwrap();
            if self.shutting_down.load(Ordering::Acquire) {
                return Err(IndexError::Closed);
            }
            if entries.contains_key(&workspace) {
                return Ok(());
            }
            let config = self.settings.lock().unwrap().clone().unwrap_or(config);
            let (config_updates, updates) = tokio::sync::watch::channel(config.clone());
            let generation = self.next.fetch_add(1, Ordering::Relaxed);
            let (owner, cancel) = CancelHandle::new();
            entries.insert(
                workspace.clone(),
                WorkspaceEntry {
                    config_updates,
                    index_worker: None,
                    index_dirty: None,
                    persistence: self.store.as_ref().map(|_| super::PersistenceStatus {
                        verifying: true,
                        ..Default::default()
                    }),
                    generation,
                    owner,
                    cancel: cancel.clone(),
                    discovery: None,
                    config: config.clone(),
                    projects: Vec::new(),
                    revision: 0,
                    stale: true,
                    watch_error: None,
                    worker: None,
                },
            );
            (generation, cancel, updates, config)
        };
        // Enable watching before discovery: queued events reconcile changes made during the scan.
        let manager = self.clone();
        let registered_workspace = workspace.clone();
        let worker = tauri::async_runtime::spawn(async move {
            let active = tokio::select! { biased; _ = cancel.cancelled() => return, result = super::watch::FileWatcher::start(&workspace, manager.servers.clone()) => result };
            let mut watcher = match active {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    manager.watch_failed(&workspace, generation, error);
                    None
                }
            };
            if let Err(error) = manager
                .refresh(&workspace, generation, &cancel, &config, &[])
                .await
            {
                manager.watch_failed(&workspace, generation, error.to_string());
                if let Some(old) = watcher.take() {
                    let _ = tokio::task::spawn_blocking(move || drop(old)).await;
                }
            } else if watcher.is_some()
                && let Some(entry) = manager
                    .workspaces
                    .lock()
                    .unwrap()
                    .get_mut(&workspace)
                    .filter(|entry| entry.generation == generation)
            {
                entry.stale = entry.config_pending();
            }
            let mut idle = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = idle.tick() => manager.servers.reap_idle().await,
                    changed = updates.changed() => {
                        if changed.is_err() { break; }
                        let config = updates.borrow_and_update().clone();
                        match manager.refresh(&workspace, generation, &cancel, &config, &[]).await {
                            Ok(()) => {
                                if let Some(entry) = manager.workspaces.lock().unwrap().get_mut(&workspace).filter(|e| e.generation == generation) {
                                    entry.stale = entry.watch_error.is_some() || entry.config_pending();
                                }
                            }
                            Err(error) => manager.watch_failed(&workspace, generation, error.to_string()),
                        }
                    },
                    batch = async {
                        match &mut watcher {
                            Some(watcher) => watcher.batch().await,
                            None => {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                match super::watch::FileWatcher::start(&workspace, manager.servers.clone()).await {
                                    Ok(active) => { watcher = Some(active); super::watch::Batch { reconcile: true, error: Some("File watcher restored; reconciling".into()), ..Default::default() } }
                                    Err(error) => super::watch::Batch { error: Some(error), ..Default::default() },
                                }
                            }
                        }
                    } => {
                        if batch.paths.is_empty() && !batch.reconcile && batch.error.is_none() { continue; }
                        if watcher.is_none() {
                            manager.watch_failed(&workspace, generation, batch.error.unwrap_or_default());
                            continue;
                        }
                        let config = updates.borrow().clone();
                        if let Err(error) = manager.update_files(&workspace, generation, &cancel, &config, batch).await {
                            manager.watch_failed(&workspace, generation, error.to_string());
                            // Retry a failed sync even when the filesystem produces no further events.
                            if let Some(old) = watcher.take() { let _ = tokio::task::spawn_blocking(move || drop(old)).await; }
                        }
                    }
                }
            }
            let _ = tokio::task::spawn_blocking(move || drop(watcher)).await;
        });
        let mut entries = self.workspaces.lock().unwrap();
        if let Some(entry) = entries
            .get_mut(&registered_workspace)
            .filter(|entry| entry.generation == generation)
        {
            entry.worker = Some(worker);
            if let Some(store) = self.store.clone() {
                let manager = self.clone();
                let root = registered_workspace.clone();
                let cancel = entry.cancel.clone();
                entry.index_worker = Some(tauri::async_runtime::spawn(async move {
                    persistence::run(manager, store, root, generation, cancel).await;
                }));
            }
        } else {
            worker.abort();
        }
        Ok(())
    }

    fn watch_failed(&self, workspace: &Path, generation: u64, error: String) {
        if let Some(entry) = self
            .workspaces
            .lock()
            .unwrap()
            .get_mut(workspace)
            .filter(|entry| entry.generation == generation)
        {
            entry.stale = true;
            self.cache.lock().unwrap().invalidate(workspace);
            entry.index_dirty = None;
            if let Some(index) = &mut entry.persistence {
                index.verifying = true;
            }
            if entry.discovery.is_none() {
                entry.discovery = Some(Discovery::failed(workspace.to_path_buf(), error.clone()));
            }
            entry.watch_error = Some(error);
        }
    }

    async fn refresh(
        self: &Arc<Self>,
        workspace: &Path,
        generation: u64,
        cancel: &Cancel,
        config: &CodeIndexConfig,
        changed: &[PathBuf],
    ) -> Result<(), IndexError> {
        let discovery = tokio::select! { biased; _ = cancel.cancelled() => return Err(IndexError::Closed), result = super::discovery::discover_with_cancel(workspace, Some(cancel.clone())) => result.map_err(IndexError::Backend)? };
        let projects = discovery.projects.clone();
        let pending = {
            let mut workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces
                .get_mut(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            let mut previous = std::mem::take(&mut entry.projects);
            entry.projects = projects
                .iter()
                .map(|project| {
                    let found = previous.iter().position(|(status, _)| {
                        status.root == project.root && status.language == project.language
                    });
                    let restart = entry.config.server_changed(config, project.language)
                        || changed.iter().any(|path| {
                            if !super::watch::configuration(path) {
                                return false;
                            }
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let language_matches = super::watch::configuration_language(path)
                                .is_none_or(|language| project.language == language);
                            language_matches
                                && (path.starts_with(&project.root)
                                    || discovery.go_members.iter().any(|(member, owner)| {
                                        owner == &project.root && path.starts_with(member)
                                    })
                                    || !name.starts_with("Cargo.")
                                        && project.root.starts_with(path.parent().unwrap_or(path)))
                        });
                    if let Some(index) = found {
                        let old = previous.remove(index);
                        if !restart && project.error.is_none() {
                            return old;
                        }
                        if let Some(key) = old.1
                            && let Some(task) = self.servers.retire(&key)
                        {
                            self.closing.lock().unwrap().push(task);
                        }
                    }
                    (
                        ProjectStatus {
                            version: None,
                            root: project.root.clone(),
                            language: project.language,
                            state: ServerState::Queued,
                            capabilities: None,
                            progress: Vec::new(),
                            generation: 0,
                        },
                        None,
                    )
                })
                .collect();
            for (_, key) in previous {
                if let Some(key) = key
                    && let Some(task) = self.servers.retire(&key)
                {
                    self.closing.lock().unwrap().push(task);
                }
            }
            entry.config = config.clone();
            entry.discovery = Some(discovery);
            projects
                .into_iter()
                .filter(|project| {
                    entry.projects.iter().any(|(status, key)| {
                        status.root == project.root
                            && status.language == project.language
                            && key.is_none()
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut tasks = tokio::task::JoinSet::new();
        for project in pending {
            if tasks.len() >= 4 {
                let _ = tasks.join_next().await;
            }
            let manager = self.clone();
            let workspace = workspace.to_path_buf();
            let config = config.clone();
            let cancel = cancel.clone();
            tasks.spawn(async move {
                let detected = tokio::select! { biased; _ = cancel.cancelled() => return, result = manager.detect_server(&project, &config) => result };
                let mut workspaces = manager.workspaces.lock().unwrap();
                let Some(entry) = workspaces.get_mut(&workspace).filter(|entry| entry.generation == generation) else { return; };
                let Some((status, key)) = entry.projects.iter_mut().find(|(status, _)| status.root == project.root && status.language == project.language) else { return; };
                if key.is_some() { return; }
                match detected {
                    Ok(Some(command)) => {
                        let registered = manager.servers.register(&workspace, &project.root, project.language, command);
                        *key = Some(registered.clone()); manager.servers.start(registered);
                    }
                    Ok(None) => status.state = ServerState::Disabled,
                    Err(error) => status.state = ServerState::Failed(error),
                }
            });
        }
        while tasks.join_next().await.is_some() {}
        Ok(())
    }

    async fn update_files(
        self: &Arc<Self>,
        workspace: &Path,
        generation: u64,
        cancel: &Cancel,
        config: &CodeIndexConfig,
        batch: super::watch::Batch,
    ) -> Result<(), IndexError> {
        let old_files = {
            let mut workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces
                .get_mut(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            entry.stale = true;
            entry.revision += 1;
            self.cache.lock().unwrap().invalidate(workspace);
            if batch.reconcile || batch.error.is_some() {
                entry.index_dirty = None;
            } else if let Some(dirty) = &mut entry.index_dirty {
                dirty.extend(batch.paths.iter().cloned());
            }
            if let Some(index) = &mut entry.persistence {
                index.verifying = true;
            }
            if let Some(error) = &batch.error {
                entry.watch_error = Some(error.clone());
            }
            entry
                .discovery
                .as_ref()
                .map(|d| d.files.clone())
                .unwrap_or_default()
        };
        let paths: Vec<_> = batch.paths.into_iter().collect();
        if batch.reconcile
            || paths
                .iter()
                .any(|path| old_files.contains(path) != path.is_file())
        {
            let mut refresh_paths = paths.clone();
            if batch.error.is_some() {
                refresh_paths.push(workspace.join(".ignore"));
            }
            self.refresh(workspace, generation, cancel, config, &refresh_paths)
                .await?;
        }
        let files = {
            let workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces
                .get(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            entry
                .discovery
                .as_ref()
                .map(|d| d.files.clone())
                .unwrap_or_default()
        };
        let mut targets: std::collections::BTreeSet<_> = paths.into_iter().collect();
        targets.extend(old_files.symmetric_difference(&files).cloned());
        if batch.error.is_some() {
            targets.extend(old_files.iter().cloned());
            targets.extend(files.iter().cloned());
        }
        let changes: Vec<_> = targets
            .into_iter()
            .filter_map(|path| {
                if !super::watch::configuration(&path)
                    && !old_files.contains(&path)
                    && !files.contains(&path)
                {
                    return None;
                }
                // Re-resolve every target before any server-side overlay read, including symlinks.
                let contained = Workspace::new(&workspace.to_string_lossy())
                    .and_then(|scope| scope.resolve(&path.to_string_lossy()))
                    .is_ok_and(|checked| checked == path);
                let kind = if !contained || !path.is_file() || !files.contains(&path) {
                    3
                } else if !old_files.contains(&path) {
                    1
                } else {
                    2
                };
                Some((path, kind))
            })
            .collect();
        tokio::select! { biased; _ = cancel.cancelled() => return Err(IndexError::Closed), result = self.servers.files_changed(workspace, &changes) => result? }
        let mut workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces
            .get_mut(workspace)
            .filter(|entry| entry.generation == generation)
            .ok_or(IndexError::Closed)?;
        entry.stale = entry.config_pending();
        entry.watch_error = None;
        Ok(())
    }

    pub fn status(&self, workspace: &Path) -> Result<WorkspaceStatus, IndexError> {
        let workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
        let mut status = WorkspaceStatus {
            workspace: workspace.to_path_buf(),
            installs: self
                .installs
                .lock()
                .unwrap()
                .iter()
                .map(|(language, job)| (*language, job.status.clone()))
                .collect(),
            cache: self.cache.lock().unwrap().status(workspace),
            persistence: entry.persistence.clone(),
            discovering: entry.discovery.is_none(),
            revision: 0,
            stale: entry.stale,
            incomplete: entry
                .discovery
                .as_ref()
                .is_some_and(|discovery| discovery.incomplete),
            warnings: entry
                .discovery
                .as_ref()
                .map(|discovery| discovery.warnings.clone())
                .unwrap_or_default()
                .into_iter()
                .chain(entry.watch_error.clone())
                .collect(),
            projects: entry
                .projects
                .iter()
                .map(|(status, key)| {
                    key.as_ref()
                        .and_then(|key| self.servers.status(key))
                        .unwrap_or_else(|| status.clone())
                })
                .collect(),
        };
        // Presentation revisions include progress and persistence changes, not just source edits.
        let bytes = serde_json::to_vec(&(&status, entry.generation, entry.revision))
            .expect("WorkspaceStatus is serializable");
        let mut snapshots = self.snapshots.lock().unwrap();
        let snapshot = snapshots
            .entry(workspace.to_path_buf())
            .or_insert_with(|| (Vec::new(), 0));
        if snapshot.0 != bytes {
            *snapshot = (bytes, self.status_revision.fetch_add(1, Ordering::Relaxed));
        }
        status.revision = snapshot.1;
        Ok(status)
    }

    fn route(
        &self,
        workspace: &Path,
        query: &Query,
    ) -> Result<(ProjectKey, PathBuf, u64, u64, Cancel), IndexError> {
        let workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
        if entry.stale {
            return Err(IndexError::NotReady);
        }
        let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
        let file = query
            .file()
            .ok_or_else(|| IndexError::Invalid("A file is required for this query.".into()))?;
        let project = discovery.route(file).map_err(IndexError::Invalid)?;
        let (status, key) = entry
            .projects
            .iter()
            .find(|(status, _)| status.root == project.root && status.language == project.language)
            .ok_or(IndexError::NotReady)?;
        let key = key.clone().ok_or_else(|| match &status.state {
            ServerState::Disabled => IndexError::Disabled,
            ServerState::Failed(error) => IndexError::Backend(error.clone()),
            _ => IndexError::NotReady,
        })?;
        let path = Workspace::new(&workspace.to_string_lossy())
            .and_then(|workspace| workspace.resolve(file))
            .map_err(IndexError::Invalid)?;
        Ok((
            key,
            path,
            entry.generation,
            entry.revision,
            entry.cancel.clone(),
        ))
    }

    async fn query_live(
        &self,
        workspace: &Path,
        query: Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        if let Query::WorkspaceSymbols {
            file: None,
            query: text,
        } = &query
        {
            return self.workspace_symbols(workspace, text, cancel).await;
        }
        let (key, path, generation, revision, lifetime) = self.route(workspace, &query)?;
        let mut result = tokio::select! { biased; _ = lifetime.cancelled() => return Err(IndexError::Closed), result = self.servers.query(&key, &path, &query, cancel) => result }?;
        {
            let entries = self.workspaces.lock().unwrap();
            let entry = entries
                .get(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            if entry.stale || entry.revision != revision {
                return Err(IndexError::NotReady);
            }
            if matches!(query, Query::WorkspaceSymbols { .. })
                && let super::QueryData::Symbols(items) = &mut result.data
            {
                let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
                let file = query.file().ok_or(IndexError::NotReady)?;
                let project = discovery.route(file).map_err(IndexError::Invalid)?;
                items.retain(|item| {
                    discovery
                        .route(&item.location.path.to_string_lossy())
                        .is_ok_and(|candidate| {
                            candidate.root == project.root && candidate.language == project.language
                        })
                });
            }
        }
        if matches!(&result.data, super::QueryData::Symbols(items) if items.is_empty())
            || matches!(&result.data, super::QueryData::Locations(items) if items.is_empty())
            || matches!(&result.data, super::QueryData::Calls(items) if items.is_empty())
        {
            return Err(IndexError::NotReady);
        }
        Ok(result)
    }

    /// Search every ready project when the caller does not pin one file.
    async fn workspace_symbols(
        &self,
        workspace: &Path,
        text: &str,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let (targets, generation, revision) = {
            let workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
            if entry.stale {
                return Err(IndexError::NotReady);
            }
            if entry
                .discovery
                .as_ref()
                .is_none_or(|discovery| discovery.projects.is_empty())
            {
                return Err(IndexError::NotReady);
            }
            let targets: Vec<_> = entry
                .projects
                .iter()
                .filter_map(|(_, key)| key.clone())
                .collect();
            if targets.is_empty() {
                return if entry
                    .projects
                    .iter()
                    .all(|(status, _)| matches!(status.state, ServerState::Disabled))
                {
                    Err(IndexError::Disabled)
                } else {
                    Err(IndexError::NotReady)
                };
            }
            (targets, entry.generation, entry.revision)
        };
        let query = Query::WorkspaceSymbols {
            file: None,
            query: text.to_string(),
        };
        let mut symbols = Vec::new();
        let mut failure = None;
        let mut answered = false;
        for key in targets {
            match self.servers.query(&key, &key.root, &query, cancel).await {
                Ok(QueryResult {
                    data: QueryData::Symbols(items),
                    ..
                }) => {
                    answered = true;
                    symbols.extend(items);
                }
                Ok(_) => answered = true,
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        if !answered {
            return Err(failure.unwrap_or(IndexError::NotReady));
        }
        {
            let workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces
                .get(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            if entry.stale || entry.revision != revision {
                return Err(IndexError::NotReady);
            }
            // Workspace-symbol responses may include configuration and generated
            // paths a file-pinned query would have filtered out.
            if let Some(discovery) = &entry.discovery {
                symbols.retain(|symbol| {
                    discovery
                        .route(&symbol.location.path.to_string_lossy())
                        .is_ok()
                });
            }
        }
        if symbols.is_empty() {
            // A ready protocol does not prove the project is indexed yet.
            return Err(IndexError::NotReady);
        }
        Ok(QueryResult {
            content_hash: None,
            source: QuerySource::Live,
            coverage: QueryCoverage {
                project_verified: true,
                semantic_ready: true,
                ..Default::default()
            },
            data: QueryData::Symbols(symbols),
            generation,
            truncated: false,
            completeness_known: false,
        })
    }

    /// Queue global settings on existing workspace workers; each worker serializes refreshes.
    pub fn configure(&self, mut config: CodeIndexConfig) -> Result<(), IndexError> {
        config.validate().map_err(IndexError::Invalid)?;
        let mut workspaces = self.workspaces.lock().unwrap();
        *self.settings.lock().unwrap() = Some(config.clone());
        for (workspace, entry) in workspaces.iter_mut() {
            if super::Language::ALL.into_iter().any(|language| {
                entry
                    .config_updates
                    .borrow()
                    .server_changed(&config, language)
            }) {
                entry.config_updates.send_replace(config.clone());
                entry.stale = true;
                entry.revision += 1;
                entry.index_dirty = None;
                self.cache.lock().unwrap().invalidate(workspace);
                if let Some(index) = &mut entry.persistence {
                    index.verifying = true;
                }
            }
        }
        Ok(())
    }

    pub async fn retry(
        &self,
        workspace: &Path,
        project_root: &Path,
        language: super::Language,
        cancel: &Cancel,
    ) -> Result<(), IndexError> {
        self.retry_project(workspace, project_root, language, cancel, false)
            .await
    }

    async fn retry_project(
        &self,
        workspace: &Path,
        project_root: &Path,
        language: super::Language,
        cancel: &Cancel,
        installed: bool,
    ) -> Result<(), IndexError> {
        let (projects, config, generation, lifetime) = {
            let workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
            if installed
                && entry
                    .config
                    .servers
                    .get(&language)
                    .is_some_and(|server| !server.enabled || server.command.is_some())
            {
                return Ok(());
            }
            let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
            let projects: Vec<_> = discovery
                .projects
                .iter()
                .filter(|project| project.root == project_root && project.language == language)
                .cloned()
                .collect();
            (
                projects,
                entry.config.clone(),
                entry.generation,
                entry.cancel.clone(),
            )
        };
        if projects.is_empty() {
            return Err(IndexError::Invalid("Unknown project root".into()));
        }
        for project in projects {
            let detected = tokio::select! {
                biased;
                _ = lifetime.cancelled() => return Err(IndexError::Closed),
                _ = cancel.cancelled() => return Err(IndexError::Cancelled),
                result = self.detect_server(&project, &config) => result,
            };
            let key = {
                let mut workspaces = self.workspaces.lock().unwrap();
                let entry = workspaces
                    .get_mut(workspace)
                    .filter(|entry| entry.generation == generation)
                    .ok_or(IndexError::Closed)?;
                if entry.config.server_changed(&config, language)
                    || entry
                        .config_updates
                        .borrow()
                        .server_changed(&config, language)
                {
                    return Err(IndexError::NotReady);
                }
                let (status, key) = entry
                    .projects
                    .iter_mut()
                    .find(|(status, _)| {
                        status.root == project.root && status.language == project.language
                    })
                    .ok_or(IndexError::NotReady)?;
                if installed && !matches!(detected, Ok(Some(_))) {
                    return Ok(());
                }
                let command = match detected {
                    Ok(Some(command)) => command,
                    Ok(None) => {
                        status.state = ServerState::Disabled;
                        return Err(IndexError::Disabled);
                    }
                    Err(error) => {
                        status.state = ServerState::Failed(error.clone());
                        return Err(IndexError::Backend(error));
                    }
                };
                let registered =
                    self.servers
                        .register(workspace, &project.root, project.language, command);
                if installed
                    && key.as_ref() == Some(&registered)
                    && !self
                        .servers
                        .status(&registered)
                        .is_some_and(|status| matches!(status.state, ServerState::Failed(_)))
                {
                    continue;
                }
                if let Some(previous) = key.replace(registered.clone())
                    && previous != registered
                    && let Some(task) = self.servers.retire(&previous)
                {
                    self.closing.lock().unwrap().push(task);
                }
                entry.revision += 1;
                self.cache.lock().unwrap().invalidate(workspace);
                if let Some(index) = &mut entry.persistence {
                    index.verifying = true;
                }
                registered
            };
            if installed {
                self.servers.start(key);
            } else {
                self.servers.restart(&key, cancel).await?;
            }
        }
        Ok(())
    }

    pub fn close_workspace(&self, workspace: &Path) {
        let mut workspaces = self.workspaces.lock().unwrap();
        if let Some(entry) = workspaces.remove(workspace) {
            if let Some(store) = self.store.clone() {
                let (entries, root, at) = (
                    self.workspaces.clone(),
                    workspace.to_path_buf(),
                    cache::now(),
                );
                self.closing
                    .lock()
                    .unwrap()
                    .push(tauri::async_runtime::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            let entries = entries.lock().unwrap();
                            store.index_touch(&root.to_string_lossy(), at)?;
                            let active = entries
                                .keys()
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect::<Vec<_>>();
                            store.index_prune(&active, cache::now(), cache::DISK_BYTES)
                        })
                        .await;
                        if !matches!(result, Ok(Ok(()))) {
                            crate::log::key(format_args!(
                                "[oaklet] Could not clean closed code index: {result:?}"
                            ));
                        }
                    }));
            }
            entry.owner.cancel();
            if let Some(worker) = entry.index_worker {
                self.closing.lock().unwrap().push(worker);
            }
            if let Some(worker) = entry.worker {
                self.closing.lock().unwrap().push(worker);
            }
        }
        self.snapshots.lock().unwrap().remove(workspace);
        self.cache.lock().unwrap().invalidate(workspace);
        let cleanup = self.servers.close_workspace(workspace);
        self.closing.lock().unwrap().push(cleanup);
    }

    pub async fn drain(&self) {
        let _drain = self.draining.lock().await;
        let closing = std::mem::take(&mut *self.closing.lock().unwrap());
        for task in closing {
            let _ = task.await;
        }
    }

    pub async fn shutdown(&self) {
        let roots: Vec<_> = {
            let entries = self.workspaces.lock().unwrap();
            self.shutting_down.store(true, Ordering::Release);
            entries.keys().cloned().collect()
        };
        let jobs = std::mem::take(&mut *self.installs.lock().unwrap());
        for job in jobs.values() {
            job.owner.cancel();
        }
        for job in jobs.into_values() {
            let _ = job.task.await;
        }
        for root in roots {
            self.close_workspace(&root);
        }
        self.drain().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_watching_blocks_queries_until_a_full_reconciliation() {
        let root = crate::tools::test_support::temp_root("index-reconcile");
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("main.ts"), "export const a = 1;").unwrap();
        let config = CodeIndexConfig {
            servers: BTreeMap::from([(
                super::super::Language::TypeScript,
                super::super::ServerConfig {
                    enabled: false,
                    ..Default::default()
                },
            )]),
        };
        let manager = Arc::new(CodeIndexManager::new());
        manager.open_workspace(&root, config.clone()).unwrap();
        tokio::time::timeout(Duration::from_secs(12), async {
            while manager.status(&root).unwrap().stale {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (generation, cancel) = {
            let entries = manager.workspaces.lock().unwrap();
            let entry = &entries[&root];
            (entry.generation, entry.cancel.clone())
        };
        manager.watch_failed(&root, generation, "Lost filesystem events".into());
        let status = manager.status(&root).unwrap();
        assert!(
            status.stale
                && status
                    .warnings
                    .iter()
                    .any(|s| s.contains("Lost filesystem"))
        );
        assert_eq!(
            manager
                .query(
                    &root,
                    Query::Symbols {
                        file: "main.ts".into()
                    },
                    &cancel
                )
                .await
                .unwrap_err(),
            IndexError::NotReady
        );
        manager
            .update_files(
                &root,
                generation,
                &cancel,
                &config,
                super::super::watch::Batch {
                    reconcile: true,
                    error: Some("overflow".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let status = manager.status(&root).unwrap();
        assert!(!status.stale && status.warnings.is_empty() && status.revision > 0);
        assert_eq!(
            manager
                .query(
                    &root,
                    Query::Symbols {
                        file: "main.ts".into()
                    },
                    &cancel
                )
                .await
                .unwrap_err(),
            IndexError::Disabled
        );
        manager.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
