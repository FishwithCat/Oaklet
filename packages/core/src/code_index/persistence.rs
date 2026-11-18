//! Workspace-owned disk verification and low-priority per-file symbol extraction.
use super::CodeIndexManager;
use crate::{
    code_index::{
        Cancel, IndexError, Language, PersistenceStatus, PersistentProjectStatus, QueryData,
        ServerState, language_servers::ProjectKey,
    },
    store::{
        Store,
        code_index::{FileRecord, ProjectRecord},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

type Hashes = BTreeMap<PathBuf, Result<(String, i64, i64), IndexError>>;

struct Inventory {
    dirty: Option<BTreeSet<PathBuf>>,
    revision: u64,
    incomplete: bool,
    projects: Vec<(ProjectRecord, Option<ProjectKey>, Vec<PathBuf>)>,
    inputs: Vec<PathBuf>,
}

fn inventory(
    manager: &CodeIndexManager,
    root: &Path,
    generation: u64,
) -> Result<Inventory, IndexError> {
    let entries = manager.workspaces.lock().unwrap();
    let entry = entries
        .get(root)
        .filter(|e| e.generation == generation)
        .ok_or(IndexError::Closed)?;
    if entry.stale {
        return Err(IndexError::NotReady);
    }
    let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
    let discovery = discovery.clone();
    let revision = entry.revision;
    let dirty = entry.index_dirty.clone();
    let keys = entry
        .projects
        .iter()
        .filter_map(|(status, key)| {
            key.clone()
                .map(|key| ((status.root.clone(), status.language), key))
        })
        .collect::<BTreeMap<_, _>>();
    let fallback =
        blake3::hash(&serde_json::to_vec(&entry.config).expect("serializable configuration"))
            .to_hex()
            .to_string();
    drop(entries);
    let disk_revision = i64::try_from(revision).map_err(|e| IndexError::Backend(e.to_string()))?;
    let inputs: Vec<_> = discovery
        .files
        .iter()
        .filter(|p| Language::for_file(p).is_some() || crate::code_index::watch::configuration(p))
        .cloned()
        .collect();
    let projects = discovery
        .projects
        .iter()
        .map(|p| {
            let key = keys.get(&(p.root.clone(), p.language)).cloned();
            let fingerprint = key
                .as_ref()
                .map(|k| k.fingerprint.clone())
                .unwrap_or_else(|| fallback.clone());
            let files = inputs
                .iter()
                .filter(|path| {
                    discovery
                        .route(&path.to_string_lossy())
                        .is_ok_and(|owner| owner.root == p.root && owner.language == p.language)
                })
                .cloned()
                .collect();
            (
                ProjectRecord {
                    id: 0,
                    root: p.root.clone(),
                    language: p.language,
                    fingerprint,
                    revision: disk_revision,
                    snapshot: String::new(),
                    verified: false,
                },
                key,
                files,
            )
        })
        .collect();
    Ok(Inventory {
        dirty,
        revision,
        incomplete: discovery.incomplete,
        projects,
        inputs,
    })
}

fn current(manager: &CodeIndexManager, root: &Path, generation: u64, revision: u64) -> bool {
    manager
        .workspaces
        .lock()
        .unwrap()
        .get(root)
        .is_some_and(|e| e.generation == generation && e.revision == revision && !e.stale)
}

/// Check generation under the same workspace lock as the short write, excluding late commits after close.
async fn write<T: Send + 'static>(
    manager: &Arc<CodeIndexManager>,
    store: &Store,
    root: &Path,
    generation: u64,
    revision: u64,
    operation: impl FnOnce(&Store) -> Result<T, String> + Send + 'static,
) -> Result<T, IndexError> {
    let (manager, store, root) = (manager.clone(), store.clone(), root.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let entries = manager.workspaces.lock().unwrap();
        let entry = entries
            .get(&root)
            .filter(|e| e.generation == generation)
            .ok_or(IndexError::Closed)?;
        if entry.stale || entry.revision != revision {
            return Err(IndexError::NotReady);
        }
        operation(&store).map_err(IndexError::Backend)
    })
    .await
    .map_err(|e| IndexError::Backend(e.to_string()))?
}

fn status(
    manager: &CodeIndexManager,
    root: &Path,
    generation: u64,
    revision: Option<u64>,
    update: impl FnOnce(&mut PersistenceStatus),
) {
    if let Some(entry) = manager
        .workspaces
        .lock()
        .unwrap()
        .get_mut(root)
        .filter(|e| {
            e.generation == generation && revision.is_none_or(|r| e.revision == r && !e.stale)
        })
        && let Some(index) = &mut entry.persistence
    {
        update(index);
    }
}

async fn hash(root: &Path, path: &Path) -> Result<(String, i64, i64), IndexError> {
    let text = crate::code_index::documents::read(root, path).await?;
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| IndexError::Backend(e.to_string()))?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64);
    let size = text.len() as i64;
    let digest =
        tokio::task::spawn_blocking(move || blake3::hash(text.as_bytes()).to_hex().to_string())
            .await
            .map_err(|e| IndexError::Backend(e.to_string()))?;
    Ok((digest, size, mtime))
}

pub(super) async fn run(
    manager: Arc<CodeIndexManager>,
    store: Store,
    root: PathBuf,
    generation: u64,
    cancel: Cancel,
) {
    let database = store.clone();
    let workspace = root.to_string_lossy().into_owned();
    match tokio::task::spawn_blocking(move || database.index_projects(&workspace)).await {
        Ok(Ok(projects)) => status(&manager, &root, generation, None, |index| {
            index.restored_projects = projects.len();
            index.projects = projects
                .into_iter()
                .map(|p| PersistentProjectStatus {
                    root: p.root,
                    language: p.language,
                    snapshot: p.snapshot,
                    verified: false,
                })
                .collect();
        }),
        error => status(&manager, &root, generation, None, |index| {
            index.error = Some(format!("Could not restore code index: {error:?}"))
        }),
    }
    let mut completed = None;
    let mut hashes = Hashes::new();
    loop {
        tokio::select! { biased; _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        let pending = manager
            .workspaces
            .lock()
            .unwrap()
            .get(&root)
            .is_some_and(|e| !e.stale && completed != Some(e.revision));
        if !pending {
            continue;
        }
        let (scanner, directory) = (manager.clone(), root.clone());
        let scanned =
            tokio::task::spawn_blocking(move || inventory(&scanner, &directory, generation)).await;
        let inputs = match scanned.unwrap_or_else(|e| Err(IndexError::Backend(e.to_string()))) {
            Ok(inputs) if completed != Some(inputs.revision) => inputs,
            Ok(_) | Err(IndexError::NotReady) => continue,
            Err(_) => break,
        };
        let revision = inputs.revision;
        match reconcile(
            &manager,
            &store,
            &root,
            generation,
            &cancel,
            inputs,
            &mut hashes,
        )
        .await
        {
            Ok(()) => {
                completed = Some(revision);
            }
            Err(IndexError::Closed | IndexError::Cancelled) => break,
            Err(IndexError::NotReady) => continue,
            Err(error) => {
                status(&manager, &root, generation, Some(revision), |index| {
                    index.error = Some(error.to_string());
                    index.verifying = true;
                    index.extracting = false;
                });
                // A later filesystem change or reopen retries storage without a write-failure loop.
                completed = Some(revision);
            }
        }
    }
}

async fn reconcile(
    manager: &Arc<CodeIndexManager>,
    store: &Store,
    root: &Path,
    generation: u64,
    cancel: &Cancel,
    inputs: Inventory,
    cache: &mut Hashes,
) -> Result<(), IndexError> {
    let revision = inputs.revision;
    status(manager, root, generation, Some(revision), |index| {
        index.verifying = true;
        index.error = None;
        index.total_files = inputs
            .projects
            .iter()
            .map(|(_, _, files)| files.len())
            .sum();
        index.files = 0;
        index.extracted_files = 0;
        index.reused_files = 0;
        index.skipped_files = 0;
    });
    let mut hashes = BTreeMap::new();
    let mut verified = !inputs.incomplete;
    for path in &inputs.inputs {
        if cancel.is_cancelled() {
            return Err(IndexError::Closed);
        }
        if !current(manager, root, generation, revision) {
            return Err(IndexError::NotReady);
        }
        let value = if inputs
            .dirty
            .as_ref()
            .is_none_or(|dirty| dirty.contains(path))
            || !cache.contains_key(path)
        {
            let value = hash(root, path).await;
            cache.insert(path.clone(), value.clone());
            value
        } else {
            cache[path].clone()
        };
        verified &= value.is_ok();
        hashes.insert(path.clone(), value);
    }
    cache.retain(|path, _| hashes.contains_key(path));
    let mut snapshot = blake3::Hasher::new();
    for (path, value) in &hashes {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| IndexError::Invalid(e.to_string()))?
            .to_string_lossy();
        for part in [
            relative.as_bytes(),
            value
                .as_ref()
                .map(|v| v.0.as_bytes())
                .unwrap_or(b"unverified"),
        ] {
            snapshot.update(&(part.len() as u64).to_le_bytes());
            snapshot.update(part);
        }
    }
    let digest = snapshot.finalize();
    let projects: Vec<_> = inputs
        .projects
        .iter()
        .map(|(project, _, files)| {
            let mut project = project.clone();
            // shortcut: include all workspace inputs; narrow dependencies if cross-project invalidation becomes costly.
            let mut hash = blake3::Hasher::new();
            hash.update(digest.as_bytes());
            hash.update(project.fingerprint.as_bytes());
            hash.update(
                project
                    .root
                    .strip_prefix(root)
                    .unwrap_or(&project.root)
                    .to_string_lossy()
                    .as_bytes(),
            );
            hash.update(&crate::store::code_index::FORMAT.to_le_bytes());
            project.snapshot = hash.finalize().to_hex().to_string();
            project.verified = false;
            (
                project,
                files
                    .iter()
                    .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
                    .collect(),
            )
        })
        .collect();
    let workspace = root.to_string_lossy().into_owned();
    let records = write(manager, store, root, generation, revision, move |store| {
        store.index_inventory(&workspace, &projects)
    })
    .await?;
    {
        let mut entries = manager.workspaces.lock().unwrap();
        let entry = entries
            .get_mut(root)
            .filter(|e| e.generation == generation && e.revision == revision && !e.stale)
            .ok_or(IndexError::NotReady)?;
        entry.index_dirty = Some(BTreeSet::new());
    }
    status(manager, root, generation, Some(revision), |index| {
        index.verifying = !verified;
        index.extracting = true;
        index.projects = records
            .iter()
            .map(|p| PersistentProjectStatus {
                root: p.root.clone(),
                language: p.language,
                snapshot: p.snapshot.clone(),
                verified: p.verified,
            })
            .collect();
    });
    let project_ids: Vec<_> = records.iter().map(|p| p.id).collect();
    for (project, (_, key, files)) in records.into_iter().zip(inputs.projects) {
        let id = project.id;
        let database = store.clone();
        let old = tokio::task::spawn_blocking(move || database.index_files(id))
            .await
            .map_err(|e| IndexError::Backend(e.to_string()))?
            .map_err(IndexError::Backend)?;
        let mut old: BTreeMap<_, _> = old.into_iter().map(|f| (f.path.clone(), f)).collect();
        for path in files {
            if cancel.is_cancelled() {
                return Err(IndexError::Closed);
            }
            if !current(manager, root, generation, revision) {
                return Err(IndexError::NotReady);
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let mut file = match &hashes[&path] {
                Ok((hash, size, mtime)) => FileRecord {
                    path: relative.clone(),
                    size: *size,
                    mtime: *mtime,
                    content_hash: hash.clone(),
                    extracted_hash: None,
                    symbols_state: "pending".into(),
                    evidence_state: "unsupported: IPC extraction scheduled for step 9".into(),
                    symbols: Vec::new(),
                },
                Err(error) => FileRecord {
                    path: relative.clone(),
                    size: 0,
                    mtime: 0,
                    content_hash: String::new(),
                    extracted_hash: None,
                    symbols_state: format!("skipped: {error}"),
                    evidence_state: "unsupported: IPC extraction scheduled for step 9".into(),
                    symbols: Vec::new(),
                },
            };
            let reused = old.remove(&relative).filter(|old| {
                !file.content_hash.is_empty()
                    && old.content_hash == file.content_hash
                    && (old.extracted_hash.as_ref() == Some(&file.content_hash)
                        || (old
                            .symbols_state
                            .starts_with("skipped: Unsupported(\"documentSymbolProvider")
                            || old
                                .symbols_state
                                .starts_with("skipped: Unsupported(\"textDocument/documentSymbol")))
            });
            if let Some(old) = reused {
                file.extracted_hash = old.extracted_hash;
                file.symbols_state = old.symbols_state;
                file.symbols = old.symbols;
                status(manager, root, generation, Some(revision), |s| {
                    s.reused_files += 1
                });
            } else {
                let pending = file.clone();
                write(manager, store, root, generation, revision, move |s| {
                    s.index_replace_file(id, &pending)
                })
                .await?;
                if file.symbols_state == "pending" {
                    match extract(
                        manager,
                        root,
                        (generation, revision),
                        key.as_ref(),
                        &path,
                        cancel,
                    )
                    .await
                    {
                        Ok((QueryData::Symbols(symbols), submitted_hash))
                            if !symbols.is_empty() =>
                        {
                            if submitted_hash != file.content_hash
                                || hash(root, &path).await?.0 != file.content_hash
                            {
                                return Err(IndexError::NotReady);
                            }
                            file.symbols = symbols;
                            file.extracted_hash = Some(file.content_hash.clone());
                            file.symbols_state = "extracted: completeness unknown".into();
                        }
                        Err(IndexError::Closed | IndexError::Cancelled) => {
                            return Err(if cancel.is_cancelled() {
                                IndexError::Closed
                            } else {
                                IndexError::NotReady
                            });
                        }
                        Err(IndexError::NotReady)
                            if !current(manager, root, generation, revision) =>
                        {
                            return Err(IndexError::NotReady);
                        }
                        Ok((QueryData::Symbols(_), _)) => {
                            file.symbols_state =
                                "skipped: empty symbols; semantic completeness unknown".into();
                        }
                        Err(error) => {
                            file.symbols_state = format!("skipped: {error:?}");
                        }
                        Ok(_) => {
                            file.symbols_state = "skipped: unexpected symbol response".into();
                        }
                    }
                }
            }
            let stored = file.clone();
            write(manager, store, root, generation, revision, move |s| {
                s.index_replace_file(id, &stored)
            })
            .await?;
            status(manager, root, generation, Some(revision), |s| {
                s.files += 1;
                if file.extracted_hash.is_some() {
                    s.extracted_files += 1;
                } else {
                    s.skipped_files += 1;
                }
            });
            tokio::task::yield_now().await;
        }
    }
    write(manager, store, root, generation, revision, move |s| {
        for id in project_ids {
            s.index_verified(id, revision as i64, verified)?;
        }
        Ok(())
    })
    .await?;
    status(manager, root, generation, Some(revision), |s| {
        s.extracting = false;
        for p in &mut s.projects {
            p.verified = verified;
        }
    });
    Ok(())
}

async fn extract(
    manager: &CodeIndexManager,
    root: &Path,
    version: (u64, u64),
    key: Option<&ProjectKey>,
    path: &Path,
    cancel: &Cancel,
) -> Result<(QueryData, String), IndexError> {
    let (generation, revision) = version;
    let key = key.ok_or_else(|| IndexError::Unsupported("No available language server".into()))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(70);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(IndexError::Busy);
        }
        if !current(manager, root, generation, revision) {
            return Err(IndexError::NotReady);
        }
        let state = manager.servers.status(key).ok_or(IndexError::Closed)?.state;
        match state {
            ServerState::Queued | ServerState::Starting => {}
            ServerState::Ready => match manager.servers.background_symbols(key, path, cancel).await
            {
                Err(IndexError::Busy | IndexError::NotReady) => {}
                result => return result.map(|(r, hash)| (r.data, hash)),
            },
            state => {
                return Err(IndexError::Unsupported(format!(
                    "Language server {state:?}"
                )));
            }
        }
        tokio::select! { biased; _ = cancel.cancelled() => return Err(IndexError::Closed), _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn closed_generation_cannot_clear_a_reopened_workspaces_index() {
        let root = crate::tools::test_support::temp_root("index-late-storage");
        let store = Store::in_memory().unwrap();
        let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
        manager.open_workspace(&root, Default::default()).unwrap();
        let generation = manager.workspaces.lock().unwrap()[&root].generation;
        manager.close_workspace(&root);
        manager.open_workspace(&root, Default::default()).unwrap();
        {
            let mut entries = manager.workspaces.lock().unwrap();
            let entry = entries.get_mut(&root).unwrap();
            entry.owner.cancel();
            entry.stale = true;
        }
        let workspace = root.to_string_lossy().into_owned();
        let p = ProjectRecord {
            id: 0,
            root: root.clone(),
            language: Language::Rust,
            fingerprint: "v1".into(),
            revision: 0,
            snapshot: "kept".into(),
            verified: false,
        };
        store.index_inventory(&workspace, &[(p, vec![])]).unwrap();
        let target = workspace.clone();
        assert_eq!(
            write(&manager, &store, &root, generation, 0, move |s| s
                .index_clear(&target))
            .await
            .unwrap_err(),
            IndexError::Closed
        );
        assert_eq!(
            store.index_projects(&workspace).unwrap()[0].snapshot,
            "kept"
        );
        manager.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
