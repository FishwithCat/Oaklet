//! Short transactions for disposable code metadata. No filesystem or LSP work here.
use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};

use super::{Store, now_ms};
use crate::code_index::{Language, Symbol};

pub(crate) const FORMAT: i64 = 1;

#[derive(Clone, Debug)]
pub(crate) struct ProjectRecord {
    pub id: i64,
    pub root: PathBuf,
    pub language: Language,
    pub fingerprint: String,
    pub revision: i64,
    pub snapshot: String,
    pub verified: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FileRecord {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub content_hash: String,
    pub extracted_hash: Option<String>,
    pub symbols_state: String,
    pub evidence_state: String,
    pub symbols: Vec<Symbol>,
}

fn error(error: impl std::fmt::Display) -> String {
    format!("Code index storage unavailable: {error}")
}

impl Store {
    pub(crate) fn index_projects(&self, workspace: &str) -> Result<Vec<ProjectRecord>, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id, root, language, fingerprint, revision, snapshot, verified FROM code_index_projects WHERE workspace = ?1 AND format_version = ?2 ORDER BY root, language").map_err(error)?;
        let rows = stmt
            .query_map(params![workspace, FORMAT], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })
            .map_err(error)?;
        let mut projects = Vec::new();
        for row in rows {
            let (id, root, language, fingerprint, revision, snapshot, verified) =
                row.map_err(error)?;
            // A bad derived row is discarded; conversation and migration errors remain fatal.
            let Ok(language) = serde_json::from_str(&language) else {
                continue;
            };
            projects.push(ProjectRecord {
                id,
                root: root.into(),
                language,
                fingerprint,
                revision,
                snapshot,
                verified,
            });
        }
        Ok(projects)
    }

    /// Reconcile project/file membership atomically, including removal of ignored or deleted files.
    pub(crate) fn index_inventory(
        &self,
        workspace: &str,
        projects: &[(ProjectRecord, Vec<String>)],
    ) -> Result<Vec<ProjectRecord>, String> {
        if projects.is_empty() {
            self.index_clear(workspace)?;
            return Ok(Vec::new());
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(error)?;
        let ids: Vec<i64> = {
            let mut stmt = tx
                .prepare("SELECT id FROM code_index_projects WHERE workspace = ?1")
                .map_err(error)?;
            stmt.query_map([workspace], |r| r.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let mut kept = Vec::new();
        let mut result = Vec::new();
        for (project, paths) in projects {
            let language = serde_json::to_string(&project.language).map_err(error)?;
            let old: Option<(i64, String, i64)> = tx.query_row("SELECT id, fingerprint, format_version FROM code_index_projects WHERE workspace=?1 AND root=?2 AND language=?3", params![workspace, project.root.to_string_lossy(), language], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(error)?;
            if let Some((id, fingerprint, format)) = &old
                && (fingerprint != &project.fingerprint || *format != FORMAT)
            {
                tx.execute("DELETE FROM code_index_files WHERE project_id=?1", [id])
                    .map_err(error)?;
                tx.execute("DELETE FROM code_index_queries WHERE project_id=?1", [id])
                    .map_err(error)?;
            }
            tx.execute("INSERT INTO code_index_projects(workspace,root,language,fingerprint,format_version,revision,snapshot,verified,last_access) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(workspace,root,language) DO UPDATE SET fingerprint=excluded.fingerprint,format_version=excluded.format_version,revision=excluded.revision,snapshot=excluded.snapshot,verified=excluded.verified,last_access=excluded.last_access", params![workspace,project.root.to_string_lossy(),language,project.fingerprint,FORMAT,project.revision,project.snapshot,project.verified,now_ms()]).map_err(error)?;
            // A new snapshot invalidates every cross-file query, even when file membership is stable.
            tx.execute("DELETE FROM code_index_queries WHERE project_id=(SELECT id FROM code_index_projects WHERE workspace=?1 AND root=?2 AND language=?3) AND snapshot<>?4", params![workspace, project.root.to_string_lossy(), language, project.snapshot]).map_err(error)?;
            let id = old.map_or_else(|| tx.last_insert_rowid(), |v| v.0);
            let mut stmt = tx
                .prepare("SELECT id,path FROM code_index_files WHERE project_id=?1")
                .map_err(error)?;
            let files = stmt
                .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            let paths: std::collections::BTreeSet<_> = paths.iter().collect();
            for (file, path) in files {
                if !paths.contains(&path) {
                    tx.execute("DELETE FROM code_index_files WHERE id=?1", [file])
                        .map_err(error)?;
                }
            }
            kept.push(id);
            result.push(ProjectRecord {
                id,
                ..project.clone()
            });
        }
        for id in ids {
            if !kept.contains(&id) {
                tx.execute("DELETE FROM code_index_projects WHERE id=?1", [id])
                    .map_err(error)?;
            }
        }
        tx.commit().map_err(error)?;
        Ok(result)
    }

    pub(crate) fn index_files(&self, project: i64) -> Result<Vec<FileRecord>, String> {
        self.index_read_files(project, None)
    }

    pub(crate) fn index_file(
        &self,
        project: i64,
        path: &str,
    ) -> Result<Option<FileRecord>, String> {
        Ok(self.index_read_files(project, Some(path))?.pop())
    }

    fn index_read_files(
        &self,
        project: i64,
        path: Option<&str>,
    ) -> Result<Vec<FileRecord>, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,path,size,mtime,content_hash,extracted_hash,symbols_state,evidence_state FROM code_index_files WHERE project_id=?1 AND (?2 IS NULL OR path=?2) ORDER BY path").map_err(error)?;
        let rows = stmt
            .query_map(params![project, path], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    FileRecord {
                        path: r.get(1)?,
                        size: r.get(2)?,
                        mtime: r.get(3)?,
                        content_hash: r.get(4)?,
                        extracted_hash: r.get(5)?,
                        symbols_state: r.get(6)?,
                        evidence_state: r.get(7)?,
                        symbols: Vec::new(),
                    },
                ))
            })
            .map_err(error)?;
        let mut files = Vec::new();
        for row in rows {
            let (id, mut file) = row.map_err(error)?;
            let mut symbols = conn.prepare("SELECT payload FROM code_index_symbols WHERE file_id=?1 AND container IS NULL ORDER BY ordinal").map_err(error)?;
            let payloads = symbols
                .query_map([id], |r| r.get::<_, String>(0))
                .map_err(error)?;
            let mut invalid = false;
            for payload in payloads {
                match serde_json::from_str(&payload.map_err(error)?) {
                    Ok(symbol) => file.symbols.push(symbol),
                    Err(_) => {
                        invalid = true;
                        break;
                    }
                }
            }
            if invalid || file.extracted_hash.is_some() && file.symbols.is_empty() {
                file.symbols.clear();
                file.extracted_hash = None;
                file.symbols_state = "pending: invalid derived symbol".into();
                conn.execute("DELETE FROM code_index_symbols WHERE file_id=?1", [id])
                    .map_err(error)?;
                conn.execute(
                    "UPDATE code_index_files SET extracted_hash=NULL,symbols_state=?2 WHERE id=?1",
                    params![id, file.symbols_state],
                )
                .map_err(error)?;
            }
            files.push(file);
        }
        Ok(files)
    }

    /// Hash, coverage and all per-file derived rows commit together, or none of them do.
    pub(crate) fn index_replace_file(&self, project: i64, file: &FileRecord) -> Result<(), String> {
        // Serialize outside the shared connection lock.
        let mut serialized = Vec::new();
        let mut pending: Vec<_> = file
            .symbols
            .iter()
            .rev()
            .map(|s| (s, None::<String>))
            .collect();
        while let Some((symbol, container)) = pending.pop() {
            serialized.push((
                symbol.name.clone(),
                symbol.name.to_lowercase(),
                symbol.kind,
                container.clone(),
                if container.is_none() {
                    serde_json::to_string(symbol).map_err(error)?
                } else {
                    serde_json::to_string(&Symbol {
                        name: symbol.name.clone(),
                        kind: symbol.kind,
                        location: symbol.location.clone(),
                        body_lines: symbol.body_lines,
                        children: Vec::new(),
                    })
                    .map_err(error)?
                },
            ));
            pending.extend(
                symbol
                    .children
                    .iter()
                    .rev()
                    .map(|s| (s, Some(symbol.name.clone()))),
            );
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(error)?;
        tx.execute("INSERT INTO code_index_files(project_id,path,size,mtime,content_hash,extracted_hash,symbols_state,evidence_state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,path) DO UPDATE SET size=excluded.size,mtime=excluded.mtime,content_hash=excluded.content_hash,extracted_hash=excluded.extracted_hash,symbols_state=excluded.symbols_state,evidence_state=excluded.evidence_state", params![project,file.path,file.size,file.mtime,file.content_hash,file.extracted_hash,file.symbols_state,file.evidence_state]).map_err(error)?;
        let id: i64 = tx
            .query_row(
                "SELECT id FROM code_index_files WHERE project_id=?1 AND path=?2",
                params![project, file.path],
                |r| r.get(0),
            )
            .map_err(error)?;
        tx.execute("DELETE FROM code_index_symbols WHERE file_id=?1", [id])
            .map_err(error)?;
        tx.execute("DELETE FROM code_index_evidence WHERE file_id=?1", [id])
            .map_err(error)?;
        for (ordinal, (name, search, kind, container, payload)) in serialized.iter().enumerate() {
            tx.execute("INSERT INTO code_index_symbols(file_id,project_id,ordinal,name,search_name,kind,container,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,project,ordinal as i64,name,search,kind,container,payload]).map_err(error)?;
        }
        tx.commit().map_err(error)
    }

    pub(crate) fn index_verified(
        &self,
        project: i64,
        revision: i64,
        verified: bool,
    ) -> Result<(), String> {
        self.connection()?
            .execute(
                "UPDATE code_index_projects SET verified=?3 WHERE id=?1 AND revision=?2",
                params![project, revision, verified],
            )
            .map(|_| ())
            .map_err(error)
    }

    pub(crate) fn index_query(
        &self,
        key: &str,
        project: &ProjectRecord,
        now: i64,
    ) -> Result<Option<(String, i64)>, String> {
        let conn = self.connection()?;
        let payload = conn.query_row("SELECT q.payload,q.created_at FROM code_index_queries q JOIN code_index_projects p ON p.id=q.project_id WHERE q.key=?1 AND q.project_id=?2 AND q.snapshot=?3 AND q.fingerprint=?4 AND q.format_version=?5 AND q.created_at>?6 AND q.created_at<=?7 AND length(CAST(q.payload AS BLOB))<=16777216 AND p.verified=1 AND p.snapshot=q.snapshot AND p.fingerprint=q.fingerprint", params![key, project.id, project.snapshot, project.fingerprint, FORMAT, now - 600_000, now], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(error)?;
        if payload.is_some() {
            conn.execute(
                "UPDATE code_index_queries SET last_access=?2 WHERE key=?1",
                params![key, now],
            )
            .map_err(error)?;
            conn.execute(
                "UPDATE code_index_projects SET last_access=?2 WHERE id=?1",
                params![project.id, now],
            )
            .map_err(error)?;
        }
        Ok(payload)
    }

    pub(crate) fn index_put_query(
        &self,
        key: &str,
        project: &ProjectRecord,
        payload: &str,
        now: i64,
    ) -> Result<(), String> {
        self.connection()?.execute("INSERT OR REPLACE INTO code_index_queries(key,project_id,snapshot,fingerprint,format_version,payload,created_at,last_access) SELECT ?1,id,snapshot,fingerprint,format_version,?3,?4,?4 FROM code_index_projects WHERE id=?2 AND revision=?5 AND snapshot=?6 AND fingerprint=?7 AND verified=1 AND format_version=?8", params![key, project.id, payload, now, project.revision, project.snapshot, project.fingerprint, FORMAT]).map(|_| ()).map_err(error)
    }

    pub(crate) fn index_remove_query(&self, key: &str) -> Result<(), String> {
        self.connection()?
            .execute("DELETE FROM code_index_queries WHERE key=?1", [key])
            .map(|_| ())
            .map_err(error)
    }

    /// Logical bytes only: never VACUUM the shared database or evict an open workspace's files.
    pub(crate) fn index_prune(
        &self,
        active: &[String],
        now: i64,
        limit: i64,
    ) -> Result<(), String> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(error)?;
        tx.execute(
            "DELETE FROM code_index_queries WHERE created_at<=?1 OR created_at>?2",
            params![now - 600_000, now],
        )
        .map_err(error)?;
        let closed: Vec<(String, i64)> = {
            let mut stmt = tx.prepare("SELECT workspace,MAX(last_access) FROM code_index_projects GROUP BY workspace ORDER BY MAX(last_access)").map_err(error)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?
        };
        for (workspace, access) in &closed {
            if !active.contains(workspace) && *access < now - 30 * 86_400_000 {
                tx.execute(
                    "DELETE FROM code_index_projects WHERE workspace=?1",
                    [workspace],
                )
                .map_err(error)?;
            }
        }
        let bytes = |tx: &rusqlite::Transaction<'_>| -> Result<i64, String> {
            tx.query_row("SELECT (SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM code_index_queries)+(SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM code_index_symbols)+(SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM code_index_evidence)+(SELECT COALESCE(SUM(length(CAST(path AS BLOB))+length(content_hash)+COALESCE(length(extracted_hash),0)+length(symbols_state)+length(evidence_state)+32),0) FROM code_index_files)+(SELECT COALESCE(SUM(length(CAST(workspace AS BLOB))+length(CAST(root AS BLOB))+length(fingerprint)+length(snapshot)+64),0) FROM code_index_projects)", [], |r| r.get(0)).map_err(error)
        };
        let mut total = bytes(&tx)?;
        if total > limit {
            let queries: Vec<(String, i64)> = {
                let mut stmt = tx.prepare("SELECT key,length(CAST(payload AS BLOB)) FROM code_index_queries ORDER BY last_access,key").map_err(error)?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(error)?
                    .collect::<Result<_, _>>()
                    .map_err(error)?
            };
            for (key, size) in queries {
                if total <= limit {
                    break;
                }
                tx.execute("DELETE FROM code_index_queries WHERE key=?1", [key])
                    .map_err(error)?;
                total -= size;
            }
            for (workspace, _) in closed {
                if total <= limit {
                    break;
                }
                if !active.contains(&workspace) {
                    tx.execute(
                        "DELETE FROM code_index_projects WHERE workspace=?1",
                        [workspace],
                    )
                    .map_err(error)?;
                    total = bytes(&tx)?;
                }
            }
        }
        tx.commit().map_err(error)
    }

    pub(crate) fn index_touch(&self, workspace: &str, at: i64) -> Result<(), String> {
        self.connection()?
            .execute(
                "UPDATE code_index_projects SET last_access=MAX(last_access,?2) WHERE workspace=?1",
                params![workspace, at],
            )
            .map(|_| ())
            .map_err(error)
    }

    pub(crate) fn index_clear(&self, workspace: &str) -> Result<(), String> {
        self.connection()?
            .execute(
                "DELETE FROM code_index_projects WHERE workspace=?1",
                [workspace],
            )
            .map(|_| ())
            .map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::Location;

    fn project(root: &str) -> ProjectRecord {
        ProjectRecord {
            id: 0,
            root: root.into(),
            language: Language::Rust,
            fingerprint: "backend-v1".into(),
            revision: 1,
            snapshot: "snapshot-v1".into(),
            verified: false,
        }
    }
    fn file(name: &str) -> FileRecord {
        FileRecord {
            path: "src/lib.rs".into(),
            size: 10,
            mtime: 1,
            content_hash: "hash".into(),
            extracted_hash: Some("hash".into()),
            symbols_state: "extracted: completeness unknown".into(),
            evidence_state: "unsupported".into(),
            symbols: vec![Symbol {
                name: name.into(),
                kind: 12,
                location: Location {
                    path: "/a/src/lib.rs".into(),
                    line: 0,
                    column: 0,
                    end_line: 0,
                    end_column: 4,
                },
                body_lines: None,
                children: vec![],
            }],
        }
    }
    #[test]
    fn file_replacement_rolls_back_and_bad_derived_json_rebuilds_only_its_file() {
        let store = Store::in_memory().unwrap();
        let projects = store
            .index_inventory("/a", &[(project("/a"), vec!["src/lib.rs".into()])])
            .unwrap();
        let id = projects[0].id;
        store.index_replace_file(id, &file("alive")).unwrap();
        store
            .connection()
            .unwrap()
            .execute_batch(include_str!("code_index_failure.sql"))
            .unwrap();
        assert!(store.index_replace_file(id, &file("fail")).is_err());
        let old = store.index_files(id).unwrap();
        assert_eq!(old[0].symbols[0].name, "alive");
        assert_eq!(old[0].extracted_hash.as_deref(), Some("hash"));
        store
            .connection()
            .unwrap()
            .execute("UPDATE code_index_symbols SET payload='invalid'", [])
            .unwrap();
        let rebuilt = store.index_files(id).unwrap();
        assert!(rebuilt[0].extracted_hash.is_none() && rebuilt[0].symbols.is_empty());
        assert!(rebuilt[0].symbols_state.contains("invalid"));
        store.index_replace_file(id, &file("recovered")).unwrap();
        assert_eq!(
            store.index_files(id).unwrap()[0].symbols[0].name,
            "recovered"
        );
    }
    #[test]
    fn pruning_and_environment_changes_are_scoped_and_leave_chat_untouched() {
        let store = Store::in_memory().unwrap();
        store.create_session("chat", "/a", 10).unwrap();
        let a = store
            .index_inventory("/a", &[(project("/a"), vec!["src/lib.rs".into()])])
            .unwrap()[0]
            .id;
        let b = store
            .index_inventory("/b", &[(project("/b"), vec!["src/lib.rs".into()])])
            .unwrap()[0]
            .id;
        store.index_replace_file(a, &file("a")).unwrap();
        store.index_replace_file(b, &file("b")).unwrap();
        store
            .index_inventory("/a", &[(project("/a"), vec![])])
            .unwrap();
        assert!(store.index_files(a).unwrap().is_empty());
        assert_eq!(store.index_files(b).unwrap().len(), 1);
        let mut changed = project("/b");
        changed.fingerprint = "backend-v2".into();
        store
            .index_inventory("/b", &[(changed, vec!["src/lib.rs".into()])])
            .unwrap();
        assert!(store.index_files(b).unwrap().is_empty());
        store.index_clear("/a").unwrap();
        assert!(store.index_projects("/a").unwrap().is_empty());
        assert_eq!(store.index_projects("/b").unwrap().len(), 1);
        assert_eq!(store.get_session("chat").unwrap().unwrap().workspace, "/a");
    }
    #[test]
    fn queries_expire_without_renewing_and_reject_stale_snapshots_environments_and_late_writes() {
        let store = Store::in_memory().unwrap();
        let mut p = store
            .index_inventory("/a", &[(project("/a"), vec![])])
            .unwrap()
            .remove(0);
        store.index_put_query("key", &p, "payload", 1000).unwrap();
        assert!(
            store.index_query("key", &p, 1000).unwrap().is_none(),
            "unverified projects cannot accept cached queries"
        );
        store.index_verified(p.id, p.revision, true).unwrap();
        store.index_put_query("key", &p, "payload", 1000).unwrap();
        assert_eq!(
            store.index_query("key", &p, 600_999).unwrap().unwrap(),
            ("payload".into(), 1000)
        );
        assert!(store.index_query("key", &p, 601_000).unwrap().is_none());
        assert!(store.index_query("key", &p, 999).unwrap().is_none());
        p.snapshot = "new".into();
        p.revision += 1;
        store.index_inventory("/a", &[(p.clone(), vec![])]).unwrap();
        assert!(store.index_query("key", &p, 1000).unwrap().is_none());
        let old = project("/a");
        store
            .index_put_query("late", &ProjectRecord { id: p.id, ..old }, "late", 1000)
            .unwrap();
        assert!(store.index_query("late", &p, 1000).unwrap().is_none());
        store.index_verified(p.id, p.revision, true).unwrap();
        store.index_put_query("new", &p, "new", 1000).unwrap();
        let mut other = p.clone();
        other.fingerprint = "toolchain-upgrade".into();
        store
            .index_inventory("/a", &[(other.clone(), vec![])])
            .unwrap();
        assert!(store.index_query("new", &other, 1000).unwrap().is_none());
    }

    #[test]
    fn budget_evicts_queries_before_closed_workspaces_and_preserves_active_indexes_and_chat() {
        let store = Store::in_memory().unwrap();
        store.create_session("chat", "/closed", 1).unwrap();
        for root in ["/open", "/closed"] {
            let p = store
                .index_inventory(root, &[(project(root), vec!["src/lib.rs".into()])])
                .unwrap()
                .remove(0);
            store.index_replace_file(p.id, &file("kept")).unwrap();
            store.index_verified(p.id, p.revision, true).unwrap();
            store
                .index_put_query(root, &p, &"x".repeat(4096), 1000)
                .unwrap();
        }
        let active = vec!["/open".into()];
        store.index_prune(&active, 1001, 3000).unwrap();
        assert!(
            !store.index_projects("/closed").unwrap().is_empty(),
            "query eviction should suffice before deleting indexes"
        );
        store.index_prune(&active, 1001, 1).unwrap();
        assert!(store.index_projects("/closed").unwrap().is_empty());
        let p = store.index_projects("/open").unwrap().remove(0);
        assert_eq!(store.index_files(p.id).unwrap().len(), 1);
        assert!(store.index_query("/open", &p, 1001).unwrap().is_none());
        assert!(store.get_session("chat").unwrap().is_some());
        let p = store
            .index_inventory("/old", &[(project("/old"), vec![])])
            .unwrap()
            .remove(0);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE code_index_projects SET last_access=0 WHERE id=?1",
                [p.id],
            )
            .unwrap();
        store
            .index_prune(&active, 31 * 86_400_000, i64::MAX)
            .unwrap();
        assert!(store.index_projects("/old").unwrap().is_empty());
        assert!(!store.index_projects("/open").unwrap().is_empty());
    }
}

#[cfg(test)]
mod hierarchy_tests {
    use super::*;
    use crate::code_index::Location;
    #[test]
    fn keeps_symbol_children_and_indexes_their_names_without_copying_nested_subtrees() {
        let store = Store::in_memory().unwrap();
        let p = ProjectRecord {
            id: 0,
            root: "/a".into(),
            language: Language::Rust,
            fingerprint: "v1".into(),
            revision: 1,
            snapshot: "s".into(),
            verified: false,
        };
        let id = store
            .index_inventory("/a", &[(p, vec!["lib.rs".into()])])
            .unwrap()[0]
            .id;
        let child = Symbol {
            name: "Child".into(),
            kind: 12,
            location: Location {
                path: "/a/lib.rs".into(),
                line: 0,
                column: 0,
                end_line: 0,
                end_column: 1,
            },
            body_lines: None,
            children: vec![],
        };
        let parent = Symbol {
            name: "Parent".into(),
            children: vec![child.clone()],
            ..child
        };
        let file = FileRecord {
            path: "lib.rs".into(),
            size: 1,
            mtime: 1,
            content_hash: "h".into(),
            extracted_hash: Some("h".into()),
            symbols_state: "extracted".into(),
            evidence_state: "unsupported".into(),
            symbols: vec![parent],
        };
        store.index_replace_file(id, &file).unwrap();
        let restored = store.index_files(id).unwrap();
        assert_eq!(restored[0].symbols[0].children[0].name, "Child");
        let conn = store.connection().unwrap();
        let (container,payload):(String,String)=conn.query_row("SELECT container,payload FROM code_index_symbols WHERE project_id=?1 AND search_name='child'",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(container, "Parent");
        assert!(
            serde_json::from_str::<Symbol>(&payload)
                .unwrap()
                .children
                .is_empty()
        );
    }
}
