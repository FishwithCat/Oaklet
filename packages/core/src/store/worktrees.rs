use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use super::{EventPayload, Store, events::append_events, now_ms};
use crate::worktrees::SessionWorktree;

impl Store {
    pub(crate) fn worktree(&self, id: &str) -> Result<SessionWorktree, String> {
        self.connection()?
            .query_row(
                "SELECT state, archived FROM session_worktrees WHERE session_id = ?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .map(|(raw, archived)| decode(&raw, archived))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn worktree_directory(&self, id: &str, project: &str) -> Result<String, String> {
        let checkout = self.worktree(id)?;
        if checkout.archived {
            return Err("This chat is archived. Unarchive it to continue.".into());
        }
        Ok(checkout.directory(project).to_owned())
    }

    pub(crate) fn save_worktree(
        &self,
        id: &str,
        checkout: &SessionWorktree,
        moved: Option<&str>,
    ) -> Result<(), String> {
        let json = serde_json::to_string(checkout).map_err(|error| error.to_string())?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO session_worktrees (session_id, state, archived) VALUES (?1, ?2, ?3)              ON CONFLICT(session_id) DO UPDATE SET state = excluded.state, archived = excluded.archived",
            params![id, json, checkout.archived],
        )
        .map_err(|error| error.to_string())?;
        if let Some(path) = moved {
            append_events(
                &tx,
                id,
                &[EventPayload::WorktreeDirectory {
                    path: path.to_owned(),
                }],
                now_ms(),
            )?;
        }
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn worktree_access(&self, project: &Path, path: &Path) -> Result<bool, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT w.state FROM session_worktrees w JOIN sessions s ON s.id = w.session_id              WHERE s.workspace = ?1 AND w.archived = 0",
        ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([project.to_string_lossy().as_ref()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let state = decode(&row.map_err(|error| error.to_string())?, false)?;
            if state
                .active
                .iter()
                .any(|entry| Path::new(&entry.path) == path || Path::new(&entry.source) == path)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn project_worktree_paths(
        &self,
        project: &Path,
    ) -> Result<HashSet<PathBuf>, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT w.state FROM session_worktrees w JOIN sessions s ON s.id = w.session_id WHERE s.workspace = ?1",
        ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([project.to_string_lossy().as_ref()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| error.to_string())?;
        let mut paths = HashSet::new();
        for row in rows {
            let state = decode(&row.map_err(|error| error.to_string())?, false)?;
            for checkout in state.active.into_iter().chain(state.created) {
                paths.insert(PathBuf::from(checkout.path));
            }
        }
        Ok(paths)
    }

    pub(crate) fn worktree_referenced(&self, path: &Path) -> Result<bool, String> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT state FROM session_worktrees WHERE archived = 0")
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        for row in rows {
            let state = decode(&row.map_err(|error| error.to_string())?, false)?;
            if state
                .active
                .iter()
                .chain(&state.created)
                .any(|entry| Path::new(&entry.path).starts_with(path))
            {
                return Ok(true);
            }
        }
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s LEFT JOIN session_worktrees w              ON w.session_id = s.id WHERE s.workspace = ?1 AND COALESCE(w.archived, 0) = 0)",
            [path.to_string_lossy().as_ref()],
            |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        Ok(exists)
    }

    pub(crate) fn archived_sessions(
        &self,
        project: &Path,
    ) -> Result<Vec<super::SessionSummary>, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.updated_at, s.turns FROM session_worktrees w              JOIN sessions s ON s.id = w.session_id WHERE s.workspace = ?1 AND w.archived = 1              ORDER BY s.updated_at DESC",
        ).map_err(|error| error.to_string())?;
        stmt.query_map([project.to_string_lossy().as_ref()], |row| {
            Ok(super::SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                turns: row.get::<_, i64>(3)?.max(0) as usize,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
    }
}

fn decode(raw: &str, archived: bool) -> Result<SessionWorktree, String> {
    let mut checkout =
        serde_json::from_str::<SessionWorktree>(raw).map_err(|error| error.to_string())?;
    checkout.archived = archived;
    Ok(checkout)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::NewUserTurn;
    use crate::worktrees::{Checkout, WaitingTurn};

    #[test]
    fn saves_waiting_input_and_checkout_without_moving_project_history() {
        let store = Store::in_memory().unwrap();
        let project = "/projects/repo";
        let directory = "/projects/repo/.agents/worktrees/fix";
        store.create_session("s", project, 1).unwrap();
        assert_eq!(store.worktree_directory("s", project).unwrap(), project);
        let turn = NewUserTurn {
            message: json!({ "role": "user", "content": "Fix login" }),
            display: Some(json!({ "text": "Fix login" })),
            title: Some("Fix login".into()),
        };
        let entry = Checkout {
            path: directory.into(),
            source: project.into(),
            common_dir: "/projects/repo/.git".into(),
            branch: Some("fix".into()),
            base_commit: Some("baseline".into()),
        };
        let mut state = SessionWorktree {
            active: vec![entry.clone()],
            created: vec![entry],
            waiting: Some(WaitingTurn {
                turn: turn.clone(),
                architecture: false,
                request: None,
            }),
            ..Default::default()
        };
        store.save_worktree("s", &state, Some(directory)).unwrap();
        assert_eq!(store.worktree_directory("s", project).unwrap(), directory);
        assert!(
            store
                .worktree_access(Path::new(project), Path::new(directory))
                .unwrap()
        );
        assert_eq!(store.list_sessions(project).unwrap()[0].title, "Fix login");
        assert!(store.list_sessions(directory).unwrap().is_empty());

        store.begin_run("s", "r", &turn, 2).unwrap();
        assert!(store.worktree("s").unwrap().waiting.is_none());
        assert_eq!(store.get_session("s").unwrap().unwrap().workspace, project);

        state = store.worktree("s").unwrap();
        state.archived = true;
        store.save_worktree("s", &state, None).unwrap();
        assert!(store.list_sessions(project).unwrap().is_empty());
        assert_eq!(
            store.archived_sessions(Path::new(project)).unwrap()[0].id,
            "s"
        );
        assert!(!store.worktree_referenced(Path::new(directory)).unwrap());
    }
}
