//! Durable chat state.
//!
//! Conversations and the lifecycle of every run live here so the UI is only a
//! view. The model-facing `messages` and the rendered `items` stay as JSON
//! columns: a session is always read as a whole, and no query needs to reach
//! inside an item.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA_VERSION: i64 = 2;

const SESSION_COLUMNS: &str = "id, workspace, title, turns, created_at, updated_at, \
     context_used, context_window, messages, items";

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS sessions (
  id             TEXT PRIMARY KEY,
  workspace      TEXT NOT NULL,
  title          TEXT NOT NULL DEFAULT '',
  turns          INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  context_used   INTEGER,
  context_window INTEGER,
  messages       TEXT NOT NULL DEFAULT '[]',
  items          TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS sessions_by_workspace ON sessions(workspace, updated_at DESC);

CREATE TABLE IF NOT EXISTS runs (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  status      TEXT NOT NULL,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER,
  error       TEXT
);
CREATE INDEX IF NOT EXISTS runs_by_session ON runs(session_id, started_at DESC);

CREATE TABLE IF NOT EXISTS session_history (
  session_id  TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  message     TEXT NOT NULL,
  archived_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
);
";

/// Token gauge persisted with a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: usize,
    pub window: usize,
}

/// A whole stored conversation, including the model-facing history.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
    pub messages: Vec<Value>,
    pub items: Vec<Value>,
}

/// A lightweight row for the history palette.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub turns: usize,
}

/// The user turn being appended before a run starts.
#[derive(Clone, Debug)]
pub struct NewUserTurn {
    pub message: Value,
    pub item: Value,
    /// Set only when this is the first user message of the session.
    pub title: Option<String>,
}

/// Everything a finished run writes back in one transaction.
#[derive(Clone, Debug)]
pub struct RunEnd {
    pub session_id: String,
    pub run_id: String,
    pub status: &'static str,
    pub finished_at: i64,
    pub messages: Vec<Value>,
    pub items: Vec<Value>,
    /// Original messages replaced by compaction during this run. They are
    /// archived in the same transaction that writes `messages`, so a failure
    /// leaves both the old history and the archive untouched.
    pub archived: Vec<Value>,
    pub context: Option<ContextUsage>,
    pub error: Option<String>,
}

/// Import payload for one workspace's old `localStorage` snapshots.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEntry {
    pub workspace: String,
    pub sessions: Vec<ImportSession>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub context: Option<ContextUsage>,
}

/// Why a stored JSON column could not be read.
fn decode(raw: &str, what: &str) -> Result<Vec<Value>, String> {
    serde_json::from_str(raw).map_err(|_| format!("This session's saved {what} could not be read."))
}

fn encode(value: &[Value]) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("Could not encode session data: {error}"))
}

/// Append replaced messages to the per-session archive. `seq` continues the
/// archive, so later compactions cannot overwrite earlier originals.
fn append_history(
    conn: &Connection,
    session_id: &str,
    messages: &[Value],
    now: i64,
) -> Result<usize, String> {
    if messages.is_empty() {
        return Ok(0);
    }

    let mut seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM session_history WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not read the archived history: {error}"))?;

    let mut insert = conn
        .prepare(
            "INSERT INTO session_history (session_id, seq, message, archived_at) \
             VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(|error| format!("Could not prepare the history archive: {error}"))?;
    for message in messages {
        seq += 1;
        let raw = serde_json::to_string(message)
            .map_err(|error| format!("Could not encode archived history: {error}"))?;
        insert
            .execute(params![session_id, seq, raw, now])
            .map_err(|error| format!("Could not archive the replaced history: {error}"))?;
    }
    Ok(messages.len())
}

type SessionRow = (
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    String,
    String,
);

fn read_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn to_session(row: SessionRow) -> Result<Session, String> {
    let (id, workspace, title, turns, _created_at, updated_at, used, window, messages, items) = row;
    Ok(Session {
        id,
        workspace,
        title,
        turns: turns.max(0) as usize,
        updated_at,
        context: match (used, window) {
            (Some(used), Some(window)) => Some(ContextUsage {
                used: used.max(0) as usize,
                window: window.max(0) as usize,
            }),
            _ => None,
        },
        messages: decode(&messages, "messages")?,
        items: decode(&items, "items")?,
    })
}

/// SQLite-backed session and run storage.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("Could not create the data directory: {error}"))?;
        }
        let conn = Connection::open(path)
            .map_err(|error| format!("Could not open the session database: {error}"))?;
        configure(&conn)?;
        migrate(&conn)?;
        restrict_permissions(path);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|error| format!("Could not open the test database: {error}"))?;
        configure(&conn)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "Session storage is unavailable.".to_string())
    }

    fn get_with(conn: &Connection, id: &str) -> Result<Option<Session>, String> {
        let row = conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                [id],
                read_session_row,
            )
            .optional()
            .map_err(|error| format!("Could not read the session: {error}"))?;
        row.map(to_session).transpose()
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>, String> {
        let conn = self.connection()?;
        Self::get_with(&conn, id)
    }

    /// Number of original messages archived after compaction.
    pub fn history_count(&self, session_id: &str) -> Result<usize, String> {
        let conn = self.connection()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_history WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the archived history: {error}"))?;
        Ok(count.max(0) as usize)
    }

    /// One 1-based page of archived originals, plus the archive size.
    pub fn read_history(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<(i64, Value)>, usize), String> {
        let conn = self.connection()?;
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_history WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the archived history: {error}"))?;
        let mut statement = conn
            .prepare(
                "SELECT seq, message FROM session_history WHERE session_id = ?1 \
                 ORDER BY seq LIMIT ?2 OFFSET ?3",
            )
            .map_err(|error| format!("Could not read the archived history: {error}"))?;
        let rows = statement
            .query_map(
                params![session_id, limit.max(1) as i64, offset.max(1) as i64 - 1],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| format!("Could not read the archived history: {error}"))?;

        let mut page = Vec::new();
        for row in rows {
            let (seq, raw) =
                row.map_err(|error| format!("Could not read the archived history: {error}"))?;
            let message = serde_json::from_str(&raw)
                .map_err(|_| "An archived message could not be read.".to_string())?;
            page.push((seq, message));
        }
        Ok((page, total.max(0) as usize))
    }

    pub fn create_session(&self, id: &str, workspace: &str, now: i64) -> Result<Session, String> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO sessions (id, workspace, title, turns, created_at, updated_at, messages, items) \
             VALUES (?1, ?2, '', 0, ?3, ?3, '[]', '[]')",
            params![id, workspace, now],
        )
        .map_err(|error| format!("Could not create the session: {error}"))?;
        Self::get_with(&conn, id)?.ok_or_else(|| "The new session disappeared.".to_string())
    }

    /// History rows for one workspace. Empty tabs stay out of the palette, and
    /// the JSON columns are never parsed here.
    pub fn list_sessions(&self, workspace: &str) -> Result<Vec<SessionSummary>, String> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT id, title, updated_at, turns FROM sessions \
                 WHERE workspace = ?1 AND turns > 0 ORDER BY updated_at DESC",
            )
            .map_err(|error| format!("Could not read session history: {error}"))?;
        let rows = statement
            .query_map([workspace], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                    turns: row.get::<_, i64>(3)?.max(0) as usize,
                })
            })
            .map_err(|error| format!("Could not read session history: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read session history: {error}"))
    }

    /// Append the user turn and register the run atomically, so a crash cannot
    /// leave a run without its prompt.
    pub fn begin_run(
        &self,
        session_id: &str,
        run_id: &str,
        turn: &NewUserTurn,
        now: i64,
    ) -> Result<Session, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not start the run: {error}"))?;
        let current = Self::get_with(&tx, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;

        let title = if current.title.is_empty() {
            turn.title.clone().unwrap_or_default()
        } else {
            current.title.clone()
        };
        let mut messages = current.messages;
        messages.push(turn.message.clone());
        let mut items = current.items;
        items.push(turn.item.clone());

        tx.execute(
            "UPDATE sessions SET messages = ?2, items = ?3, title = ?4, \
             turns = turns + 1, updated_at = ?5 WHERE id = ?1",
            params![session_id, encode(&messages)?, encode(&items)?, title, now],
        )
        .map_err(|error| format!("Could not save the message: {error}"))?;
        tx.execute(
            "INSERT INTO runs (id, session_id, status, started_at) VALUES (?1, ?2, 'running', ?3)",
            params![run_id, session_id, now],
        )
        .map_err(|error| format!("Could not record the run: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not start the run: {error}"))?;

        Self::get_with(&conn, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())
    }

    /// Write the finished turn and its run status in one transaction.
    pub fn finish_run(&self, end: &RunEnd) -> Result<Session, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not finish the run: {error}"))?;
        let changed = tx
            .execute(
                "UPDATE sessions SET messages = ?2, items = ?3, context_used = ?4, \
                 context_window = ?5, updated_at = ?6 WHERE id = ?1",
                params![
                    end.session_id,
                    encode(&end.messages)?,
                    encode(&end.items)?,
                    end.context.map(|context| context.used as i64),
                    end.context.map(|context| context.window as i64),
                    end.finished_at
                ],
            )
            .map_err(|error| format!("Could not save the session: {error}"))?;
        if changed == 0 {
            return Err("This session no longer exists.".into());
        }
        append_history(&tx, &end.session_id, &end.archived, end.finished_at)?;
        tx.execute(
            "UPDATE runs SET status = ?2, finished_at = ?3, error = ?4 WHERE id = ?1",
            params![end.run_id, end.status, end.finished_at, end.error],
        )
        .map_err(|error| format!("Could not update the run: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not finish the run: {error}"))?;

        Self::get_with(&conn, &end.session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())
    }

    /// Replace the model history and append the compaction notice.
    pub fn apply_compaction(
        &self,
        session_id: &str,
        messages: &[Value],
        replaced: &[Value],
        notice: &Value,
        context: ContextUsage,
        now: i64,
    ) -> Result<Session, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not save the compacted context: {error}"))?;
        let current = Self::get_with(&tx, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        let mut items = current.items;
        items.push(notice.clone());
        append_history(&tx, session_id, replaced, now)?;
        tx.execute(
            "UPDATE sessions SET messages = ?2, items = ?3, context_used = ?4, \
             context_window = ?5, updated_at = ?6 WHERE id = ?1",
            params![
                session_id,
                encode(messages)?,
                encode(&items)?,
                context.used as i64,
                context.window as i64,
                now
            ],
        )
        .map_err(|error| format!("Could not save the compacted context: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not save the compacted context: {error}"))?;

        Self::get_with(&conn, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())
    }

    /// Runs cannot resume after a restart; mark them and leave a visible notice
    /// instead of a transcript that stops without explanation.
    pub fn interrupt_stale_runs(&self, now: i64, notice: &Value) -> Result<usize, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not recover old runs: {error}"))?;
        let stale: Vec<(String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, session_id FROM runs WHERE status = 'running'")
                .map_err(|error| format!("Could not read old runs: {error}"))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|error| format!("Could not read old runs: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read old runs: {error}"))?
        };

        for (run_id, session_id) in &stale {
            tx.execute(
                "UPDATE runs SET status = 'interrupted', finished_at = ?2 WHERE id = ?1",
                params![run_id, now],
            )
            .map_err(|error| format!("Could not recover an old run: {error}"))?;

            let raw: Option<String> = tx
                .query_row(
                    "SELECT items FROM sessions WHERE id = ?1",
                    [session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("Could not recover a session: {error}"))?;
            let Some(raw) = raw else { continue };
            let mut items = match decode(&raw, "items") {
                Ok(items) => items,
                Err(_) => continue,
            };
            items.push(notice.clone());
            tx.execute(
                "UPDATE sessions SET items = ?2, updated_at = ?3 WHERE id = ?1",
                params![session_id, encode(&items)?, now],
            )
            .map_err(|error| format!("Could not recover a session: {error}"))?;
        }

        tx.commit()
            .map_err(|error| format!("Could not recover old runs: {error}"))?;
        Ok(stale.len())
    }

    /// One-time import of the history that used to live in `localStorage`.
    /// Ids already present win, so running the import twice is harmless.
    pub fn import_sessions(&self, entries: &[ImportEntry]) -> Result<usize, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not import old sessions: {error}"))?;
        let mut imported = 0;

        for entry in entries {
            for session in &entry.sessions {
                let turns = session
                    .items
                    .iter()
                    .filter(|item| item.get("kind").and_then(Value::as_str) == Some("user"))
                    .count();
                let changed = tx
                    .execute(
                        "INSERT OR IGNORE INTO sessions \
                         (id, workspace, title, turns, created_at, updated_at, context_used, \
                          context_window, messages, items) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            session.id,
                            entry.workspace,
                            session.title,
                            turns as i64,
                            session.updated_at,
                            session.context.map(|context| context.used as i64),
                            session.context.map(|context| context.window as i64),
                            encode(&session.messages)?,
                            encode(&session.items)?
                        ],
                    )
                    .map_err(|error| format!("Could not import an old session: {error}"))?;
                imported += changed;
            }
        }

        tx.commit()
            .map_err(|error| format!("Could not import old sessions: {error}"))?;
        Ok(imported)
    }
}

fn configure(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
    )
    .map_err(|error| format!("Could not configure the session database: {error}"))
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("Could not read the session database version: {error}"))?;
    if version > SCHEMA_VERSION {
        return Err(
            "This Oaklet data was created by a newer version. Update Oaklet to open it.".into(),
        );
    }
    if version < SCHEMA_VERSION {
        let tx = conn
            .unchecked_transaction()
            .map_err(|error| format!("Could not prepare the session database: {error}"))?;
        tx.execute_batch(SCHEMA)
            .map_err(|error| format!("Could not create the session tables: {error}"))?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| format!("Could not version the session database: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not create the session tables: {error}"))?;
    }
    Ok(())
}

fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
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
mod tests {
    use serde_json::json;

    use super::{ContextUsage, ImportEntry, ImportSession, NewUserTurn, RunEnd, Store, now_ms};

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    fn turn(text: &str, title: Option<&str>) -> NewUserTurn {
        NewUserTurn {
            message: json!({ "role": "user", "content": text }),
            item: json!({ "kind": "user", "text": text }),
            title: title.map(str::to_string),
        }
    }

    #[test]
    fn migrates_once_and_reopens_without_losing_rows() {
        let root = crate::tools::test_support::temp_root("store-migrate");
        let path = root.join("oaklet.db");
        {
            let store = Store::open(&path).unwrap();
            store.create_session("s1", "/tmp/ws", 1).unwrap();
        }
        let reopened = Store::open(&path).unwrap();
        assert!(reopened.get_session("s1").unwrap().is_some());
        let version: i64 = reopened
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn adds_the_history_archive_to_a_version_one_database() {
        let root = crate::tools::test_support::temp_root("store-migrate-v1");
        let path = root.join("oaklet.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                   id TEXT PRIMARY KEY,
                   workspace TEXT NOT NULL,
                   title TEXT NOT NULL DEFAULT '',
                   turns INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   context_used INTEGER,
                   context_window INTEGER,
                   messages TEXT NOT NULL DEFAULT '[]',
                   items TEXT NOT NULL DEFAULT '[]'
                 );
                 INSERT INTO sessions (id, workspace, created_at, updated_at)
                 VALUES ('old', '/tmp/a', 1, 1);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert!(store.get_session("old").unwrap().is_some());
        assert_eq!(store.history_count("old").unwrap(), 0);
        let version: i64 = store
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn creates_lists_and_scopes_sessions_by_workspace() {
        let store = store();
        store.create_session("empty", "/tmp/a", 1).unwrap();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store.create_session("s2", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("one", Some("one")), 2)
            .unwrap();
        store
            .begin_run("s2", "r2", &turn("two", Some("two")), 3)
            .unwrap();
        store.create_session("other", "/tmp/b", 4).unwrap();
        store.create_session("s3", "/tmp/b", 4).unwrap();
        store
            .begin_run("s3", "r3", &turn("three", Some("three")), 5)
            .unwrap();

        let list = store.list_sessions("/tmp/a").unwrap();
        assert_eq!(
            list.iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            ["s2", "s1"]
        );
        assert_eq!(list[0].turns, 1);
        assert_eq!(store.list_sessions("/tmp/missing").unwrap().len(), 0);
    }

    #[test]
    fn begin_run_appends_the_turn_and_keeps_the_first_title() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        let first = store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();
        assert_eq!(first.title, "hello");
        assert_eq!(first.turns, 1);
        assert_eq!(first.messages.len(), 1);
        assert_eq!(first.items.len(), 1);

        let second = store
            .begin_run("s1", "r2", &turn("again", Some("again")), 3)
            .unwrap();
        assert_eq!(second.title, "hello");
        assert_eq!(second.turns, 2);
        assert_eq!(second.messages.len(), 2);
    }

    #[test]
    fn finish_run_writes_the_transcript_context_and_status() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();
        let saved = store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                status: "done",
                finished_at: 9,
                messages: vec![
                    json!({ "role": "user", "content": "hello" }),
                    json!({ "role": "assistant", "content": "hi" }),
                ],
                items: vec![
                    json!({ "kind": "user", "text": "hello" }),
                    json!({ "kind": "assistant", "runId": "r1", "text": "hi", "streaming": false }),
                ],
                archived: vec![json!({ "role": "user", "content": "older" })],
                context: Some(ContextUsage {
                    used: 12,
                    window: 100,
                }),
                error: None,
            })
            .unwrap();

        assert_eq!(saved.messages.len(), 2);
        assert_eq!(saved.items.len(), 2);
        assert_eq!(
            saved.context,
            Some(ContextUsage {
                used: 12,
                window: 100
            })
        );
        assert_eq!(saved.updated_at, 9);
        assert_eq!(store.history_count("s1").unwrap(), 1);
        let (history, total) = store.read_history("s1", 1, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(history[0].1["content"], "older");

        let conn = store.connection().unwrap();
        let status: (String, Option<i64>) = conn
            .query_row(
                "SELECT status, finished_at FROM runs WHERE id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, ("done".into(), Some(9)));
    }

    #[test]
    fn marks_old_running_runs_interrupted_and_adds_a_notice() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();

        let recovered = store
            .interrupt_stale_runs(
                5,
                &json!({ "kind": "notice", "text": "interrupted", "tone": "error" }),
            )
            .unwrap();
        assert_eq!(recovered, 1);

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.items.len(), 2);
        assert_eq!(session.items[1]["kind"], "notice");
        assert_eq!(session.updated_at, 5);

        let conn = store.connection().unwrap();
        let status: String = conn
            .query_row("SELECT status FROM runs WHERE id = 'r1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "interrupted");
        // A second startup finds nothing left to recover.
        drop(conn);
        assert!(
            store
                .interrupt_stale_runs(
                    6,
                    &json!({ "kind": "notice", "text": "x", "tone": "muted" })
                )
                .unwrap()
                == 0
        );
    }

    #[test]
    fn imports_legacy_snapshots_once() {
        let store = store();
        let entries = vec![ImportEntry {
            workspace: "/tmp/a".into(),
            sessions: vec![
                ImportSession {
                    id: "old".into(),
                    title: "Old chat".into(),
                    updated_at: 7,
                    messages: vec![json!({ "role": "user", "content": "hi" })],
                    items: vec![
                        json!({ "kind": "user", "text": "hi" }),
                        json!({ "kind": "assistant", "runId": "r", "text": "hello", "streaming": false }),
                    ],
                    context: Some(ContextUsage {
                        used: 10,
                        window: 100,
                    }),
                },
                ImportSession {
                    id: "old".into(),
                    title: "Duplicate".into(),
                    updated_at: 8,
                    messages: vec![],
                    items: vec![],
                    context: None,
                },
            ],
        }];

        assert_eq!(store.import_sessions(&entries).unwrap(), 1);
        assert_eq!(store.import_sessions(&entries).unwrap(), 0);

        let session = store.get_session("old").unwrap().unwrap();
        assert_eq!(session.title, "Old chat");
        assert_eq!(session.turns, 1);
        assert_eq!(session.updated_at, 7);
        assert_eq!(
            session.context,
            Some(ContextUsage {
                used: 10,
                window: 100
            })
        );
        assert_eq!(store.list_sessions("/tmp/a").unwrap().len(), 1);
    }

    #[test]
    fn compacts_in_place_and_keeps_the_old_items() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();
        let session = store
            .apply_compaction(
                "s1",
                &[json!({ "role": "user", "content": "summary" })],
                &[
                    json!({ "role": "user", "content": "old request" }),
                    json!({ "role": "assistant", "content": "old answer" }),
                ],
                &json!({ "kind": "notice", "text": "compacted", "tone": "muted" }),
                ContextUsage {
                    used: 5,
                    window: 100,
                },
                4,
            )
            .unwrap();

        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.items.len(), 2);
        assert_eq!(
            session.context,
            Some(ContextUsage {
                used: 5,
                window: 100
            })
        );
        let (history, total) = store.read_history("s1", 1, 10).unwrap();
        assert_eq!(total, 2);
        assert_eq!(history[0].1["content"], "old request");
        assert_eq!(history[1].1["content"], "old answer");
        assert!(now_ms() > 0);
    }
}
