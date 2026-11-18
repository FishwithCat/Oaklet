//! Only verified file symbols are reusable; current backends have no semantic-readiness proof.
use super::*;
use crate::{
    code_index::{CacheStatus, QueryCoverage, QueryData, QuerySource},
    store::{Store, code_index::FORMAT},
};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) const DISK_BYTES: i64 = 256 * 1024 * 1024;
const TTL: i64 = 600_000;

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

struct Cached {
    workspace: PathBuf,
    result: QueryResult,
    created: i64,
    used: u64,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct QueryCache {
    entries: HashMap<String, Cached>,
    sequence: u64,
    bytes: usize,
    errors: BTreeMap<PathBuf, String>,
}

impl QueryCache {
    pub(super) fn invalidate(&mut self, workspace: &Path) {
        self.entries.retain(|_, entry| entry.workspace != workspace);
        self.bytes = self.entries.values().map(|e| e.bytes).sum();
        self.errors.remove(workspace);
    }
    pub(super) fn status(&self, workspace: &Path) -> CacheStatus {
        CacheStatus {
            entries: self
                .entries
                .values()
                .filter(|e| e.workspace == workspace)
                .count(),
            bytes: self
                .entries
                .values()
                .filter(|e| e.workspace == workspace)
                .map(|e| e.bytes)
                .sum(),
            error: self.errors.get(workspace).cloned(),
            semantic_cache_enabled: false,
        }
    }
    fn remove(&mut self, key: &str) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes -= entry.bytes;
        }
    }
    fn get(&mut self, key: &str, at: i64) -> Option<QueryResult> {
        let entry = self.entries.get(key)?;
        if at < entry.created || at - entry.created >= TTL {
            self.remove(key);
            return None;
        }
        self.sequence += 1;
        let entry = self.entries.get_mut(key)?;
        entry.used = self.sequence;
        Some(entry.result.clone())
    }
    fn put(&mut self, key: String, workspace: PathBuf, result: QueryResult, created: i64) {
        // Empty, failed and semantically unverified cross-file responses never enter either cache.
        if !matches!(&result.data, QueryData::Symbols(s) if !s.is_empty())
            || !result.coverage.file_verified
        {
            return;
        }
        let Ok(payload) = serde_json::to_vec(&result) else {
            return;
        };
        let bytes = payload.len();
        if bytes > MAX_BYTES {
            return;
        }
        self.remove(&key);
        self.sequence += 1;
        self.entries.insert(
            key,
            Cached {
                workspace,
                result,
                created,
                used: self.sequence,
                bytes,
            },
        );
        self.bytes += bytes;
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }
    }
}

struct Context {
    root: PathBuf,
    path: PathBuf,
    project: PathBuf,
    language: super::super::Language,
    fingerprint: Option<String>,
    snapshot: Option<String>,
    generation: u64,
    revision: u64,
    server_generation: u64,
    lifetime: Cancel,
}

impl CodeIndexManager {
    fn cache_context(&self, root: &Path, query: &Query) -> Result<Context, IndexError> {
        let entries = self.workspaces.lock().unwrap();
        let entry = entries.get(root).ok_or(IndexError::Closed)?;
        if entry.stale {
            return Err(IndexError::NotReady);
        }
        let discovery = entry.discovery.as_ref().ok_or(IndexError::NotReady)?;
        let project = discovery.route(query.file()).map_err(IndexError::Invalid)?;
        let (status, key) = entry
            .projects
            .iter()
            .find(|(s, _)| s.root == project.root && s.language == project.language)
            .ok_or(IndexError::NotReady)?;
        if matches!(status.state, ServerState::Disabled) {
            return Err(IndexError::Disabled);
        }
        let path = Workspace::new(&root.to_string_lossy())
            .and_then(|w| w.resolve(query.file()))
            .map_err(IndexError::Invalid)?;
        let snapshot = entry
            .persistence
            .as_ref()
            .filter(|p| !p.verifying)
            .and_then(|p| {
                p.projects.iter().find(|p| {
                    p.root == project.root && p.language == project.language && p.verified
                })
            })
            .map(|p| p.snapshot.clone());
        Ok(Context {
            root: root.into(),
            path,
            project: project.root.clone(),
            language: project.language,
            fingerprint: key.as_ref().map(|k| k.fingerprint.clone()),
            snapshot,
            generation: entry.generation,
            revision: entry.revision,
            server_generation: key
                .as_ref()
                .and_then(|k| self.servers.status(k))
                .map_or(0, |s| s.generation),
            lifetime: entry.cancel.clone(),
        })
    }

    fn cache_current(&self, context: &Context, cancel: &Cancel) -> Result<(), IndexError> {
        if cancel.is_cancelled() {
            return Err(IndexError::Cancelled);
        }
        let entries = self.workspaces.lock().unwrap();
        let entry = entries
            .get(&context.root)
            .filter(|e| e.generation == context.generation)
            .ok_or(IndexError::Closed)?;
        if entry.stale || entry.revision != context.revision {
            return Err(IndexError::NotReady);
        }
        Ok(())
    }

    fn cache_error(&self, context: &Context, error: impl ToString) {
        let entries = self.workspaces.lock().unwrap();
        if entries
            .get(&context.root)
            .is_some_and(|e| e.generation == context.generation && e.revision == context.revision)
        {
            self.cache
                .lock()
                .unwrap()
                .errors
                .insert(context.root.clone(), error.to_string());
        }
    }

    pub async fn query(
        &self,
        workspace: &Path,
        query: Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let root = Workspace::new(&workspace.to_string_lossy())
            .map_err(IndexError::Invalid)?
            .root()
            .to_path_buf();
        // Never turn a successful-but-partial LSP response into an authoritative cached call graph.
        if !matches!(query, Query::Symbols { .. }) {
            return self.query_live(&root, query, cancel).await;
        }
        let context = self.cache_context(&root, &query)?;
        self.cache_current(&context, cancel)?;
        let work = self.query_symbols(&context, query, cancel);
        tokio::select! { biased; _ = cancel.cancelled() => Err(IndexError::Cancelled), _ = context.lifetime.cancelled() => Err(IndexError::Closed), result = work => result }
    }

    async fn query_symbols(
        &self,
        context: &Context,
        query: Query,
        cancel: &Cancel,
    ) -> Result<QueryResult, IndexError> {
        let hash = file_hash(context).await?;
        let mut key = context
            .snapshot
            .as_ref()
            .zip(context.fingerprint.as_ref())
            .map(|(snapshot, fingerprint)| {
                query_key(context, &query, snapshot, fingerprint, &hash)
            });
        let memory = key
            .as_ref()
            .and_then(|k| self.cache.lock().unwrap().get(k, now()));
        let mut project = None;
        let mut result = memory.map(|mut r| {
            r.source = QuerySource::Memory;
            r
        });
        if result.is_none()
            && let Some(store) = &self.store
        {
            let (database, root) = (store.clone(), context.root.to_string_lossy().into_owned());
            match blocking(move || database.index_projects(&root)).await {
                Ok(projects) => {
                    project = projects.into_iter().find(|p| {
                        p.root == context.project
                            && p.language == context.language
                            && context
                                .fingerprint
                                .as_ref()
                                .is_none_or(|f| f == &p.fingerprint)
                    })
                }
                Err(error) => self.cache_error(context, error),
            }
            if let Some(p) = &project {
                if let Some(key) = &key {
                    let (database, p, k) = (store.clone(), p.clone(), key.clone());
                    match blocking(move || database.index_query(&k, &p, now())).await {
                        Ok(Some((payload, created))) => {
                            match serde_json::from_str::<QueryResult>(&payload) {
                                Ok(mut r)
                                    if matches!(&r.data, QueryData::Symbols(s) if !s.is_empty())
                                        && r.coverage.file_verified
                                        && local_symbols(&r, &context.path) =>
                                {
                                    r.source = QuerySource::Sqlite;
                                    {
                                        let entries = self.workspaces.lock().unwrap();
                                        if entries.get(&context.root).is_some_and(|e| {
                                            e.generation == context.generation
                                                && e.revision == context.revision
                                                && !e.stale
                                        }) {
                                            self.cache.lock().unwrap().put(
                                                key.clone(),
                                                context.root.clone(),
                                                r.clone(),
                                                created,
                                            );
                                        }
                                    }
                                    result = Some(r);
                                }
                                _ => {
                                    let (database, k) = (store.clone(), key.clone());
                                    if let Err(error) =
                                        blocking(move || database.index_remove_query(&k)).await
                                    {
                                        self.cache_error(context, error);
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => self.cache_error(context, error),
                    }
                }
                if result.is_none() {
                    let (database, id, path) = (
                        store.clone(),
                        p.id,
                        context
                            .path
                            .strip_prefix(&context.root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                    match blocking(move || database.index_file(id, &path)).await {
                        Ok(Some(file))
                            if file.extracted_hash.as_ref() == Some(&hash)
                                && file.content_hash == hash
                                && !file.symbols.is_empty() =>
                        {
                            result = Some(QueryResult {
                                content_hash: Some(hash.clone()),
                                source: QuerySource::FileIndex,
                                coverage: QueryCoverage {
                                    file_verified: true,
                                    project_verified: context.snapshot.is_some(),
                                    semantic_ready: false,
                                },
                                data: QueryData::Symbols(file.symbols),
                                generation: context.server_generation,
                                truncated: false,
                                completeness_known: false,
                            });
                            if !local_symbols(result.as_ref().unwrap(), &context.path) {
                                result = None;
                            }
                        }
                        Ok(Some(file)) if file.content_hash != hash => {
                            key = None;
                        }
                        Ok(_) => {}
                        Err(error) => self.cache_error(context, error),
                    }
                }
            }
        }
        let mut result = match result {
            Some(result) => result,
            None => self.query_live(&context.root, query, cancel).await?,
        };
        if file_hash(context).await? != hash
            || result.source == QuerySource::Live && result.content_hash.as_ref() != Some(&hash)
        {
            return Err(IndexError::NotReady);
        }
        self.cache_current(context, cancel)?;
        if result.source != QuerySource::Live {
            result.generation = context.server_generation;
        }
        result.coverage.file_verified = local_symbols(&result, &context.path);
        result.coverage.project_verified = key.is_some();
        if let Some(key) = key.filter(|_| local_symbols(&result, &context.path)) {
            // Guard insertion with the same workspace lock used by close, rebuild and file invalidation.
            {
                let entries = self.workspaces.lock().unwrap();
                if !entries.get(&context.root).is_some_and(|e| {
                    e.generation == context.generation && e.revision == context.revision && !e.stale
                }) {
                    return Err(IndexError::NotReady);
                }
                // Preserve original TTL on memory/disk hits; a read must never renew a stale result.
                if !matches!(result.source, QuerySource::Memory | QuerySource::Sqlite) {
                    self.cache.lock().unwrap().put(
                        key.clone(),
                        context.root.clone(),
                        result.clone(),
                        now(),
                    );
                }
            }
            if matches!(result.source, QuerySource::Live | QuerySource::FileIndex)
                && let (Some(store), Some(project)) = (&self.store, project)
            {
                let payload = serde_json::to_string(&result)
                    .map_err(|e| IndexError::Backend(e.to_string()))?;
                if payload.len() <= MAX_BYTES {
                    let (entries, database, root, generation, revision) = (
                        self.workspaces.clone(),
                        store.clone(),
                        context.root.clone(),
                        context.generation,
                        context.revision,
                    );
                    let write = blocking(move || {
                        let entries = entries.lock().unwrap();
                        if !entries.get(&root).is_some_and(|e| {
                            e.generation == generation && e.revision == revision && !e.stale
                        }) {
                            return Ok(());
                        }
                        database.index_put_query(&key, &project, &payload, now())?;
                        let active = entries
                            .keys()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>();
                        database.index_prune(&active, now(), DISK_BYTES)
                    })
                    .await;
                    if let Err(error) = write {
                        self.cache_error(context, error);
                    }
                }
            }
        }
        self.cache_current(context, cancel)?;
        Ok(result)
    }
}

fn query_key(
    context: &Context,
    query: &Query,
    snapshot: &str,
    fingerprint: &str,
    hash: &str,
) -> String {
    let mut query = query.clone();
    match &mut query {
        Query::Symbols { file }
        | Query::Definition { file, .. }
        | Query::References { file, .. }
        | Query::Calls { file, .. } => {
            *file = context
                .path
                .strip_prefix(&context.root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        }
    }
    let payload = serde_json::to_vec(&(
        context.root.to_string_lossy(),
        context.project.to_string_lossy(),
        context.language,
        query,
        snapshot,
        fingerprint,
        hash,
        FORMAT,
    ))
    .expect("query key contains serializable business values");
    blake3::hash(&payload).to_hex().to_string()
}

async fn file_hash(context: &Context) -> Result<String, IndexError> {
    let text = super::super::documents::read(&context.root, &context.path).await?;
    tokio::task::spawn_blocking(move || blake3::hash(text.as_bytes()).to_hex().to_string())
        .await
        .map_err(|e| IndexError::Backend(e.to_string()))
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, IndexError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|e| IndexError::Backend(e.to_string()))?
        .map_err(IndexError::Backend)
}

pub(super) async fn prune(
    manager: &Arc<CodeIndexManager>,
    store: &Store,
) -> Result<(), IndexError> {
    let (manager, store) = (manager.clone(), store.clone());
    blocking(move || {
        let entries = manager.workspaces.lock().unwrap();
        let active = entries
            .keys()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        store.index_prune(&active, now(), DISK_BYTES)
    })
    .await
}

impl CodeIndexManager {
    /// Clear only this workspace's derived rows, then restart discovery and server sessions.
    pub async fn rebuild(self: &Arc<Self>, workspace: &Path) -> Result<(), IndexError> {
        let root = Workspace::new(&workspace.to_string_lossy())
            .map_err(IndexError::Invalid)?
            .root()
            .to_path_buf();
        let manager = self.clone();
        // The transition must finish even when a window drops its awaiting future.
        tauri::async_runtime::spawn(async move { manager.rebuild_owned(root).await })
            .await
            .map_err(|e| IndexError::Backend(e.to_string()))?
    }

    async fn rebuild_owned(self: &Arc<Self>, root: PathBuf) -> Result<(), IndexError> {
        let (generation, config) = {
            let mut entries = self.workspaces.lock().unwrap();
            let entry = entries.get_mut(&root).ok_or(IndexError::Closed)?;
            entry.generation = self.next.fetch_add(1, Ordering::Relaxed);
            entry.revision += 1;
            entry.stale = true;
            entry.owner.cancel();
            for task in [entry.worker.take(), entry.index_worker.take()]
                .into_iter()
                .flatten()
            {
                self.closing.lock().unwrap().push(task);
            }
            self.cache.lock().unwrap().invalidate(&root);
            self.closing
                .lock()
                .unwrap()
                .push(self.servers.close_workspace(&root));
            (entry.generation, entry.config.clone())
        };
        let cleared = if let Some(store) = &self.store {
            let (entries, database, root) = (self.workspaces.clone(), store.clone(), root.clone());
            blocking(move || {
                let entries = entries.lock().unwrap();
                if !entries
                    .get(&root)
                    .is_some_and(|e| e.generation == generation)
                {
                    return Err("Workspace closed during rebuild".into());
                }
                database.index_clear(&root.to_string_lossy())
            })
            .await
        } else {
            Ok(())
        };
        {
            let mut entries = self.workspaces.lock().unwrap();
            if !entries
                .get(&root)
                .is_some_and(|e| e.generation == generation)
            {
                return Err(IndexError::Closed);
            }
            entries.remove(&root);
        }
        // Reopen even on a storage failure so a failed rebuild never disables live queries.
        self.open_workspace(&root, config)?;
        cleared
    }
}

fn local_symbols(result: &QueryResult, path: &Path) -> bool {
    fn local(symbol: &crate::code_index::Symbol, path: &Path) -> bool {
        symbol.location.path == path && symbol.children.iter().all(|s| local(s, path))
    }
    matches!(&result.data, QueryData::Symbols(symbols) if !symbols.is_empty() && symbols.iter().all(|s| local(s, path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(path: &Path) -> QueryResult {
        QueryResult {
            content_hash: None,
            source: QuerySource::Live,
            coverage: QueryCoverage {
                file_verified: true,
                ..Default::default()
            },
            generation: 1,
            truncated: true,
            completeness_known: false,
            data: QueryData::Symbols(vec![crate::code_index::Symbol {
                name: "symbol".into(),
                kind: 12,
                location: crate::code_index::Location {
                    path: path.into(),
                    line: 0,
                    column: 0,
                    end_line: 0,
                    end_column: 1,
                },
                children: vec![],
            }]),
        }
    }
    #[test]
    fn cache_enforces_lru_payload_limit_ttl_and_no_negative_or_semantic_entries() {
        let root = PathBuf::from("/workspace");
        let mut cache = QueryCache::default();
        for i in 0..MAX_ENTRIES {
            cache.put(
                i.to_string(),
                root.clone(),
                result(&root.join("a.rs")),
                1000,
            );
        }
        assert!(cache.get("0", 1001).is_some());
        cache.put(
            "extra".into(),
            root.clone(),
            result(&root.join("a.rs")),
            1001,
        );
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.get("1", 1002).is_none());
        assert!(cache.get("0", 1000 + TTL).is_none());
        let mut no = result(&root);
        no.data = QueryData::Symbols(vec![]);
        cache.put("empty".into(), root.clone(), no.clone(), 1000);
        no.data = QueryData::Locations(vec![crate::code_index::Location {
            path: root.clone(),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 1,
        }]);
        no.completeness_known = true;
        cache.put("semantic".into(), root.clone(), no, 1000);
        assert!(!cache.entries.contains_key("empty") && !cache.entries.contains_key("semantic"));
        let mut large = result(&root);
        if let QueryData::Symbols(s) = &mut large.data {
            s[0].name = "x".repeat(MAX_BYTES / 2);
        }
        for i in 0..3 {
            cache.put(format!("large{i}"), root.clone(), large.clone(), 1000);
        }
        assert!(cache.bytes <= MAX_BYTES);
        assert!(cache.entries.len() < 3);
        cache.invalidate(&root);
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
    }
    #[test]
    fn keys_normalize_paths_and_bind_query_options_environment_snapshot_and_format() {
        let (_owner, lifetime) = CancelHandle::new();
        let c = Context {
            root: "/a".into(),
            path: "/a/src/lib.rs".into(),
            project: "/a".into(),
            language: crate::code_index::Language::Rust,
            fingerprint: Some("backend".into()),
            snapshot: Some("snapshot".into()),
            generation: 1,
            revision: 1,
            server_generation: 1,
            lifetime,
        };
        let relative = Query::Symbols {
            file: "src/./lib.rs".into(),
        };
        let absolute = Query::Symbols {
            file: "/a/src/lib.rs".into(),
        };
        let key = query_key(&c, &relative, "s1", "v1", "h1");
        assert_eq!(key, query_key(&c, &absolute, "s1", "v1", "h1"));
        assert_ne!(key, query_key(&c, &relative, "s2", "v1", "h1"));
        assert_ne!(key, query_key(&c, &relative, "s1", "v2", "h1"));
        assert_ne!(key, query_key(&c, &relative, "s1", "v1", "h2"));
        let incoming = Query::Calls {
            file: "src/lib.rs".into(),
            line: 1,
            column: 2,
            incoming: true,
        };
        let outgoing = Query::Calls {
            file: "src/lib.rs".into(),
            line: 1,
            column: 2,
            incoming: false,
        };
        assert_ne!(
            query_key(&c, &incoming, "s1", "v1", "h1"),
            query_key(&c, &outgoing, "s1", "v1", "h1")
        );
        assert!(!local_symbols(&result(Path::new("/outside")), &c.path));
    }
}
