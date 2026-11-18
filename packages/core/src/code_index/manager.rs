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
                },
            );
            (generation, cancel)
        };
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let discovery = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = super::discovery::discover_with_cancel(&workspace, Some(cancel.clone())) => result,
            };
            match discovery {
                Ok(discovery) => {
                    let projects = discovery.projects.clone();
                    {
                        let mut workspaces = manager.workspaces.lock().unwrap();
                        let Some(entry) = workspaces
                            .get_mut(&workspace)
                            .filter(|entry| entry.generation == generation)
                        else {
                            return;
                        };
                        entry.discovery = Some(discovery);
                        entry.projects = projects
                            .iter()
                            .map(|project| {
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
                    }
                    // Detection for one missing backend must not prevent another from starting.
                    let mut tasks = tokio::task::JoinSet::new();
                    for project in projects {
                        if tasks.len() >= 4 {
                            let _ = tasks.join_next().await;
                        }
                        let manager = manager.clone();
                        let workspace = workspace.clone();
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
                                    *key = Some(registered.clone());
                                    manager.servers.start(registered);
                                }
                                Ok(None) => status.state = ServerState::Disabled,
                                Err(error) => status.state = ServerState::Failed(error),
                            }
                        });
                    }
                    while tasks.join_next().await.is_some() {}
                }
                Err(error) => {
                    let mut workspaces = manager.workspaces.lock().unwrap();
                    if let Some(entry) = workspaces
                        .get_mut(&workspace)
                        .filter(|entry| entry.generation == generation)
                    {
                        entry.discovery = Some(Discovery::failed(workspace.clone(), error));
                    }
                }
            }
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(15)) => manager.servers.reap_idle().await,
                }
            }
        });
        Ok(())
    }

    pub fn status(&self, workspace: &Path) -> Result<WorkspaceStatus, IndexError> {
        let workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
        Ok(WorkspaceStatus {
            discovering: entry.discovery.is_none(),
            incomplete: entry
                .discovery
                .as_ref()
                .is_some_and(|discovery| discovery.incomplete),
            warnings: entry
                .discovery
                .as_ref()
                .map(|discovery| discovery.warnings.clone())
                .unwrap_or_default(),
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
    ) -> Result<(ProjectKey, PathBuf, u64, Cancel), IndexError> {
        let workspaces = self.workspaces.lock().unwrap();
        let entry = workspaces.get(workspace).ok_or(IndexError::Closed)?;
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
        Ok((key, path, entry.generation, entry.cancel.clone()))
    }

    pub async fn query(
        &self,
        workspace: &Path,
        query: Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let (key, path, generation, lifetime) = self.route(workspace, &query)?;
        let result = tokio::select! { biased; _ = lifetime.cancelled() => return Err(IndexError::Closed), result = self.servers.query(&key, &path, &query, cancel) => result }?;
        if !self
            .workspaces
            .lock()
            .unwrap()
            .get(workspace)
            .is_some_and(|entry| entry.generation == generation)
        {
            return Err(IndexError::Closed);
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
