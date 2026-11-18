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
    CodeIndexConfig, Discovery, IndexError, ProjectStatus, Query, QueryResult, ServerState,
    WorkspaceStatus, detect_server,
    language_servers::{LanguageServerManager, ProjectKey},
};
use crate::tools::{Cancel, CancelHandle, Workspace};

struct WorkspaceEntry {
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

/// Shared Rust service; no protocol or model-specific types cross this boundary.
pub struct CodeIndexManager {
    workspaces: Mutex<BTreeMap<PathBuf, WorkspaceEntry>>,
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
    pub fn new() -> Self {
        Self {
            workspaces: Mutex::new(BTreeMap::new()),
            servers: LanguageServerManager::new(),
            next: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
            closing: Mutex::new(Vec::new()),
            draining: tokio::sync::Mutex::new(()),
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
        let (generation, cancel) = {
            let mut entries = self.workspaces.lock().unwrap();
            if self.shutting_down.load(Ordering::Acquire) {
                return Err(IndexError::Closed);
            }
            if entries.contains_key(&workspace) {
                return Ok(());
            }
            let generation = self.next.fetch_add(1, Ordering::Relaxed);
            let (owner, cancel) = CancelHandle::new();
            entries.insert(
                workspace.clone(),
                WorkspaceEntry {
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
            (generation, cancel)
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
                entry.stale = false;
            }
            let mut idle = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = idle.tick() => manager.servers.reap_idle().await,
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
                    let restart = changed.iter().any(|path| {
                        if !super::watch::configuration(path) {
                            return false;
                        }
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let language_matches = if name.starts_with("Cargo.") {
                            project.language == super::Language::Rust
                        } else if name == ".gitignore" || name == ".ignore" {
                            true
                        } else {
                            project.language == super::Language::TypeScript
                        };
                        language_matches
                            && (path.starts_with(&project.root)
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
                let detected = tokio::select! { biased; _ = cancel.cancelled() => return, result = detect_server(&project, &config) => result };
                let mut workspaces = manager.workspaces.lock().unwrap();
                let Some(entry) = workspaces.get_mut(&workspace).filter(|entry| entry.generation == generation) else { return; };
                let Some((status, key)) = entry.projects.iter_mut().find(|(status, _)| status.root == project.root && status.language == project.language) else { return; };
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
        entry.stale = false;
        entry.watch_error = None;
        Ok(())
    }

    pub fn status(&self, workspace: &Path) -> Result<WorkspaceStatus, IndexError> {
        let workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
        Ok(WorkspaceStatus {
            discovering: entry.discovery.is_none(),
            revision: entry.revision,
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
        })
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
        let project = discovery.route(query.file()).map_err(IndexError::Invalid)?;
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
            .and_then(|workspace| workspace.resolve(query.file()))
            .map_err(IndexError::Invalid)?;
        Ok((
            key,
            path,
            entry.generation,
            entry.revision,
            entry.cancel.clone(),
        ))
    }

    pub async fn query(
        &self,
        workspace: &Path,
        query: Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let (key, path, generation, revision, lifetime) = self.route(workspace, &query)?;
        let result = tokio::select! { biased; _ = lifetime.cancelled() => return Err(IndexError::Closed), result = self.servers.query(&key, &path, &query, cancel) => result }?;
        {
            let entries = self.workspaces.lock().unwrap();
            let entry = entries
                .get(workspace)
                .filter(|entry| entry.generation == generation)
                .ok_or(IndexError::Closed)?;
            if entry.stale || entry.revision != revision {
                return Err(IndexError::NotReady);
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

    pub async fn retry(
        &self,
        workspace: &Path,
        project_root: &Path,
        cancel: &Cancel,
    ) -> Result<(), IndexError> {
        let (projects, config, generation, lifetime) = {
            let workspaces = self.workspaces.lock().unwrap();
            let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
            let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
            let projects: Vec<_> = discovery
                .projects
                .iter()
                .filter(|project| project.root == project_root)
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
                result = detect_server(&project, &config) => result,
            };
            let key = {
                let mut workspaces = self.workspaces.lock().unwrap();
                let entry = workspaces
                    .get_mut(workspace)
                    .filter(|entry| entry.generation == generation)
                    .ok_or(IndexError::Closed)?;
                let (status, key) = entry
                    .projects
                    .iter_mut()
                    .find(|(status, _)| {
                        status.root == project.root && status.language == project.language
                    })
                    .ok_or(IndexError::NotReady)?;
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
                *key = Some(registered.clone());
                registered
            };
            self.servers.restart(&key, cancel).await?;
        }
        Ok(())
    }

    pub fn close_workspace(&self, workspace: &Path) {
        let mut workspaces = self.workspaces.lock().unwrap();
        if let Some(entry) = workspaces.remove(workspace) {
            entry.owner.cancel();
            if let Some(worker) = entry.worker {
                self.closing.lock().unwrap().push(worker);
            }
        }
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
