use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::Store;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUsage {
    pub workspace: String,
    pub label: String,
    pub sessions: usize,
    pub messages: usize,
    pub bytes: u64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreUsage {
    pub database_bytes: u64,
    pub sessions: usize,
    pub messages: usize,
    pub workspaces: Vec<WorkspaceUsage>,
}

impl Store {
    pub fn has_workspace(&self, workspace: &str) -> Result<bool, String> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT 1 FROM sessions WHERE workspace = ?1 LIMIT 1",
            [workspace],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(|error| format!("Could not read storage usage: {error}"))
    }

    pub fn usage(&self) -> Result<StoreUsage, String> {
        let conn = self.connection()?;
        let workspaces = {
            let mut statement = conn
                .prepare(
                    "SELECT s.workspace, count(DISTINCT s.id), count(e.seq), \
                     coalesce(sum(length(e.payload)), 0), max(s.updated_at) \
                     FROM sessions s LEFT JOIN session_events e ON e.session_id = s.id \
                     GROUP BY s.workspace ORDER BY 4 DESC, s.workspace",
                )
                .map_err(|error| format!("Could not read storage usage: {error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok(WorkspaceUsage {
                        workspace: row.get(0)?,
                        label: String::new(),
                        sessions: row.get::<_, i64>(1)?.max(0) as usize,
                        messages: row.get::<_, i64>(2)?.max(0) as usize,
                        bytes: row.get::<_, i64>(3)?.max(0) as u64,
                        updated_at: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    })
                })
                .map_err(|error| format!("Could not read storage usage: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read storage usage: {error}"))?
        };
        Ok(StoreUsage {
            database_bytes: self.disk_bytes(),
            sessions: workspaces.iter().map(|row| row.sessions).sum(),
            messages: workspaces.iter().map(|row| row.messages).sum(),
            workspaces,
        })
    }

    pub fn purge(&self, workspace: Option<&str>) -> Result<Vec<String>, String> {
        let conn = self.connection()?;
        let ids = {
            let mut statement = conn
                .prepare("SELECT id FROM sessions WHERE ?1 IS NULL OR workspace = ?1")
                .map_err(|error| format!("Could not read the saved sessions: {error}"))?;
            let rows = statement
                .query_map(params![workspace], |row| row.get(0))
                .map_err(|error| format!("Could not read the saved sessions: {error}"))?;
            rows.collect::<Result<Vec<String>, _>>()
                .map_err(|error| format!("Could not read the saved sessions: {error}"))?
        };
        conn.execute(
            "DELETE FROM sessions WHERE ?1 IS NULL OR workspace = ?1",
            params![workspace],
        )
        .map_err(|error| format!("Could not delete the saved history: {error}"))?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .map_err(|error| format!("Could not reclaim the database file: {error}"))?;
        Ok(ids)
    }

    fn disk_bytes(&self) -> u64 {
        let Some(path) = &self.path else {
            return 0;
        };
        ["", "-wal", "-shm"]
            .into_iter()
            .filter_map(|suffix| {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                std::fs::metadata(PathBuf::from(name)).ok()
            })
            .map(|metadata| metadata.len())
            .sum()
    }
}
