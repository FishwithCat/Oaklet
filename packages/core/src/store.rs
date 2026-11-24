//! Durable chat state.
//!
//! `session_events` is the only conversation store: an append-only log of
//! messages, notices, and compaction records. The model-facing history and the
//! rendered transcript are both projections of that log. Tool trims append
//! model-only replacements while preserving the original transcript. The UI reads bounded windows;
//! a run start reads only the post-compaction model tail.

mod events;
mod history;
mod maintenance;
mod migrations;
mod projection;
mod runs;
mod sessions;
mod types;

use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};

#[cfg(test)]
pub(crate) use events::tool_trims;
pub(crate) use events::{checkpoint, is_checkpoint};
pub use maintenance::WorkspaceUsage;
#[cfg(test)]
pub use types::CompactionRecord;
pub use types::{
    CompactionEnd, ContextUsage, Event, EventPayload, NewUserTurn, PlanDetails, PlanRecord,
    PlanState, PlanStatus, PlanStep, PlanValidation, PlanValidationStatus, RunEnd, RunStart,
    RunUsage, Session, SessionMeta, SessionSummary, SessionWindow, StepUsage,
};

/// Events in one UI window. The model surface never uses this page size.
pub const PAGE_EVENTS: usize = 200;

/// SQLite-backed session and run storage.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("Could not create the data directory: {error}"))?;
            // Keep the database, WAL, SHM, and spill siblings private even if
            // SQLite creates the journal files with the process umask.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        // Create the main database owner-only before SQLite can create a wider
        // file. An existing file keeps its mode here and is narrowed below.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path);
        }
        let conn = Connection::open(path)
            .map_err(|error| format!("Could not open the session database: {error}"))?;
        configure(&conn)?;
        migrations::migrate(&conn)?;
        restrict_permissions(path);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(path.to_path_buf()),
        })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|error| format!("Could not open the test database: {error}"))?;
        configure(&conn)?;
        migrations::migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "Session storage is unavailable.".to_string())
    }
}

fn configure(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
    )
    .map_err(|error| format!("Could not configure the session database: {error}"))
}

fn has_table(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(|error| format!("Could not inspect the session database: {error}"))
}

fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        for suffix in ["-wal", "-shm"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let _ = std::fs::set_permissions(
                std::path::PathBuf::from(name),
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests;
