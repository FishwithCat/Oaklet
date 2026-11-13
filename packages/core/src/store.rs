//! Durable chat state.
//!
//! `session_events` is the only conversation store: an append-only log of
//! messages, notices, and compaction records. The model-facing history and the
//! rendered transcript are both projections of that log, so neither is written
//! a second time and a turn only appends rows. The UI reads bounded windows;
//! a run start reads only the post-compaction model tail.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA_VERSION: i64 = 5;

/// Events in one UI window. The model surface never uses this page size.
pub const PAGE_EVENTS: usize = 200;

const SESSION_COLUMNS: &str = "id, workspace, title, turns, created_at, updated_at, \
     context_used, context_window";

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS sessions (
  id             TEXT PRIMARY KEY,
  workspace      TEXT NOT NULL,
  title          TEXT NOT NULL DEFAULT '',
  turns          INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  context_used   INTEGER,
  context_window INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_by_workspace ON sessions(workspace, updated_at DESC);

CREATE TABLE IF NOT EXISTS session_events (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL,
  payload    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS runs (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  status      TEXT NOT NULL,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER,
  error       TEXT
);
CREATE INDEX IF NOT EXISTS runs_by_session ON runs(session_id, started_at DESC);
";

/// One append-only transcript entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// A model-visible message. `display` carries UI-only data and never
    /// reaches a model request.
    Message {
        message: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<Value>,
    },
    /// User-visible text that is not part of the conversation.
    Notice { tone: String, text: String },
    /// A summary replacing the transcript prefix through `shadowed_through`.
    Compaction {
        summary: Value,
        shadowed_through: i64,
        before: usize,
        after: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub payload: EventPayload,
    pub created_at: i64,
}

/// Token gauge persisted with a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: usize,
    pub window: usize,
}

/// A stored conversation: metadata plus its append-only event log.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
    pub events: Vec<Event>,
}

/// A session's metadata without its event log.
#[derive(Clone, Debug)]
pub struct SessionMeta {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
}

/// One contiguous slice of a session log, newest window or run delta.
#[derive(Clone, Debug)]
pub struct SessionWindow {
    pub id: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
    pub events: Vec<Event>,
    /// Whether events exist before the first event in this window.
    pub has_more: bool,
}

/// A started run: the new UI window, its first event, and the model history
/// that preceded it. The history is read inside the append transaction.
#[derive(Clone, Debug)]
pub struct RunStart {
    pub window: SessionWindow,
    pub start_seq: i64,
    pub prior_messages: Vec<Value>,
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
    /// Model-facing message, with no display-only fields.
    pub message: Value,
    /// UI-only metadata such as attachment thumbnails and the text to show.
    pub display: Option<Value>,
    /// Set only when this is the first user message of the session.
    pub title: Option<String>,
}

/// Everything a finished run appends in one transaction.
#[derive(Clone, Debug)]
pub struct RunEnd {
    pub session_id: String,
    pub run_id: String,
    /// Seq of the user event appended by `begin_run`.
    pub start_seq: i64,
    pub status: &'static str,
    pub finished_at: i64,
    pub appended: Vec<EventPayload>,
    /// Present when this run compacted its history.
    pub compaction: Option<CompactionEnd>,
    pub context: Option<ContextUsage>,
    pub error: Option<String>,
}

/// A compaction record to append after this run's messages.
#[derive(Clone, Debug)]
pub struct CompactionEnd {
    pub summary: Value,
    /// Model history after compaction; the retained tail determines the
    /// shadow boundary.
    pub surface: Vec<Value>,
    pub before: usize,
    pub after: usize,
}

/// One compaction write: summary, boundary, and the gauge it produced.
#[derive(Clone, Debug)]
pub struct CompactionRecord {
    pub summary: Value,
    pub shadowed_through: i64,
    pub before: usize,
    pub after: usize,
    pub context: ContextUsage,
}

fn decode_event(seq: i64, raw: &str, created_at: i64) -> Result<Event, String> {
    let payload = serde_json::from_str(raw)
        .map_err(|_| "A transcript event could not be read.".to_string())?;
    Ok(Event {
        seq,
        payload,
        created_at,
    })
}

fn read_events(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Event>, String> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| format!("Could not read the transcript: {error}"))?;
    let rows = statement
        .query_map(params, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| format!("Could not read the transcript: {error}"))?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, raw, created_at) =
            row.map_err(|error| format!("Could not read the transcript: {error}"))?;
        events.push(decode_event(seq, &raw, created_at)?);
    }
    Ok(events)
}

fn decode_events(conn: &Connection, session_id: &str) -> Result<Vec<Event>, String> {
    read_events(
        conn,
        "SELECT seq, payload, created_at FROM session_events \
         WHERE session_id = ?1 ORDER BY seq",
        &[&session_id],
    )
}

/// A page is cut before it is aligned, so a turn with more than one page still
/// starts where the page starts; every smaller turn starts on its user message.
fn align_page(events: &mut Vec<Event>) {
    let Some(index) = events.iter().position(is_visible_user_event) else {
        return;
    };
    // Keep notices directly above the turn (compaction markers, interruption
    // errors); drop a partial assistant/tool block instead.
    let mut start = index;
    while start > 0
        && matches!(
            events[start - 1].payload,
            EventPayload::Notice { .. } | EventPayload::Compaction { .. }
        )
    {
        start -= 1;
    }
    if start > 0 {
        events.drain(0..start);
    }
}

fn is_visible_user_event(event: &Event) -> bool {
    let EventPayload::Message { message, display } = &event.payload else {
        return false;
    };
    message.get("role").and_then(Value::as_str) == Some("user")
        && !display
            .as_ref()
            .and_then(|display| display.get("hidden"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && !is_checkpoint(message)
}

fn read_page(
    conn: &Connection,
    session_id: &str,
    before_seq: Option<i64>,
    limit: usize,
) -> Result<Vec<Event>, String> {
    let limit = limit.max(1);
    let fetch = limit.saturating_add(1).min(i64::MAX as usize) as i64;
    let mut events = if let Some(before) = before_seq {
        read_events(
            conn,
            "SELECT seq, payload, created_at FROM session_events \
             WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
            &[&session_id, &before, &fetch],
        )?
    } else {
        read_events(
            conn,
            "SELECT seq, payload, created_at FROM session_events \
             WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2",
            &[&session_id, &fetch],
        )?
    };
    events.reverse();
    if events.len() > limit {
        events.remove(0);
    }
    if events.first().is_some_and(|event| event.seq > 1) {
        align_page(&mut events);
    }
    Ok(events)
}

fn read_from(conn: &Connection, session_id: &str, start_seq: i64) -> Result<Vec<Event>, String> {
    read_events(
        conn,
        "SELECT seq, payload, created_at FROM session_events \
         WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq",
        &[&session_id, &start_seq],
    )
}

fn next_seq(conn: &Connection, session_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_events WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .map_err(|error| format!("Could not read the transcript: {error}"))
}

fn append_events(
    conn: &Connection,
    session_id: &str,
    payloads: &[EventPayload],
    now: i64,
) -> Result<(), String> {
    if payloads.is_empty() {
        return Ok(());
    }
    let start = next_seq(conn, session_id)?;
    let mut insert = conn
        .prepare(
            "INSERT INTO session_events (session_id, seq, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(|error| format!("Could not append to the transcript: {error}"))?;
    for (index, payload) in payloads.iter().enumerate() {
        let raw = serde_json::to_string(payload)
            .map_err(|error| format!("Could not encode a transcript event: {error}"))?;
        insert
            .execute(params![session_id, start + index as i64, raw, now])
            .map_err(|error| format!("Could not append to the transcript: {error}"))?;
    }
    Ok(())
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
    ))
}

fn to_meta(row: SessionRow) -> SessionMeta {
    let (id, workspace, title, turns, _created_at, updated_at, used, window) = row;
    SessionMeta {
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
    }
}

fn to_session(conn: &Connection, row: SessionRow) -> Result<Session, String> {
    let meta = to_meta(row);
    let events = decode_events(conn, &meta.id)?;
    Ok(Session {
        id: meta.id,
        title: meta.title,
        turns: meta.turns,
        updated_at: meta.updated_at,
        context: meta.context,
        events,
    })
}

fn latest_compaction(conn: &Connection, session_id: &str) -> Result<Option<(Value, i64)>, String> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT payload FROM session_events \
             WHERE session_id = ?1 AND json_extract(payload, '$.type') = 'compaction' \
             ORDER BY seq DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not read the transcript: {error}"))?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    match decode_event(0, &raw, 0)?.payload {
        EventPayload::Compaction {
            summary,
            shadowed_through,
            ..
        } => Ok(Some((summary, shadowed_through))),
        _ => Ok(None),
    }
}

fn surface_messages_with(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<Vec<Value>>, String> {
    if Store::get_meta_with(conn, session_id)?.is_none() {
        return Ok(None);
    }
    let mut messages = Vec::new();
    let mut start = 0;
    if let Some((summary, through)) = latest_compaction(conn, session_id)? {
        messages.push(summary);
        start = through;
    }
    let mut statement = conn
        .prepare(
            "SELECT payload FROM session_events \
             WHERE session_id = ?1 AND seq > ?2 \
               AND json_extract(payload, '$.type') = 'message' \
             ORDER BY seq",
        )
        .map_err(|error| format!("Could not read the transcript: {error}"))?;
    let rows = statement
        .query_map(params![session_id, start], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not read the transcript: {error}"))?;
    for row in rows {
        let raw = row.map_err(|error| format!("Could not read the transcript: {error}"))?;
        let EventPayload::Message { message, .. } = decode_event(0, &raw, 0)?.payload else {
            continue;
        };
        messages.push(message);
    }
    Ok(Some(messages))
}

/// A compaction checkpoint is user-visible history, not conversation.
pub(crate) fn is_checkpoint(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.contains("<compacted-summary>")
                    || text.contains("The earlier part of this conversation was dropped")
            })
}

/// Seq of the last message a compaction summary replaces. The retained tail is
/// matched from the end, so notices do not shift the boundary.
pub(crate) fn shadow_for(events: &[Event], surface: &[Value]) -> i64 {
    let pairs: Vec<(i64, &Value)> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::Message { message, .. } => Some((event.seq, message)),
            _ => None,
        })
        .collect();
    let retained: Vec<&Value> = surface
        .iter()
        .skip_while(|message| is_checkpoint(message))
        .collect();
    if retained.is_empty() {
        return pairs.last().map(|(seq, _)| *seq).unwrap_or(0);
    }
    if retained.len() > pairs.len() {
        return 0;
    }
    // The surface tail can sit before trailing events that were not persisted
    // as model messages (a cancelled tool call, for example), so search for the
    // latest place it matches instead of assuming it ends at the last event.
    for end in (retained.len()..=pairs.len()).rev() {
        let start = end - retained.len();
        if pairs[end - 1].1 != retained[retained.len() - 1] {
            continue;
        }
        if pairs[start..end]
            .iter()
            .zip(&retained)
            .all(|(pair, expected)| pair.1 == *expected)
        {
            return if start == 0 { 0 } else { pairs[start - 1].0 };
        }
    }
    0
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
        row.map(|row| to_session(conn, row)).transpose()
    }

    fn get_meta_with(conn: &Connection, id: &str) -> Result<Option<SessionMeta>, String> {
        conn.query_row(
            &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
            [id],
            read_session_row,
        )
        .optional()
        .map(|row| row.map(to_meta))
        .map_err(|error| format!("Could not read the session: {error}"))
    }

    fn get_window_with(
        conn: &Connection,
        id: &str,
        before_seq: Option<i64>,
        limit: usize,
    ) -> Result<Option<SessionWindow>, String> {
        let Some(meta) = Self::get_meta_with(conn, id)? else {
            return Ok(None);
        };
        let events = read_page(conn, id, before_seq, limit)?;
        let has_more = events.first().is_some_and(|event| event.seq > 1);
        Ok(Some(SessionWindow {
            id: meta.id,
            title: meta.title,
            turns: meta.turns,
            updated_at: meta.updated_at,
            context: meta.context,
            events,
            has_more,
        }))
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>, String> {
        let conn = self.connection()?;
        Self::get_with(&conn, id)
    }

    /// Metadata for one session, without reading its event log.
    pub fn session_meta(&self, id: &str) -> Result<Option<SessionMeta>, String> {
        let conn = self.connection()?;
        Self::get_meta_with(&conn, id)
    }

    /// One UI page, ending just before `before_seq` when supplied.
    pub fn get_session_window(
        &self,
        id: &str,
        before_seq: Option<i64>,
        limit: usize,
    ) -> Result<Option<SessionWindow>, String> {
        let conn = self.connection()?;
        Self::get_window_with(&conn, id, before_seq, limit)
    }

    /// Number of messages a compaction summary replaced.
    pub fn history_count(&self, session_id: &str) -> Result<usize, String> {
        let conn = self.connection()?;
        let Some((_, through)) = latest_compaction(&conn, session_id)? else {
            return Ok(0);
        };
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_events \
                 WHERE session_id = ?1 AND seq <= ?2 \
                   AND json_extract(payload, '$.type') = 'message'",
                params![session_id, through],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        Ok(count.max(0) as usize)
    }

    /// One 1-based page of messages replaced by compaction.
    pub fn read_history(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<(i64, Value)>, usize), String> {
        let conn = self.connection()?;
        let Some((_, through)) = latest_compaction(&conn, session_id)? else {
            return Ok((Vec::new(), 0));
        };
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_events \
                 WHERE session_id = ?1 AND seq <= ?2 \
                   AND json_extract(payload, '$.type') = 'message'",
                params![session_id, through],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let start = offset.max(1) - 1;
        let mut statement = conn
            .prepare(
                "SELECT seq, payload FROM session_events \
                 WHERE session_id = ?1 AND seq <= ?2 \
                   AND json_extract(payload, '$.type') = 'message' \
                 ORDER BY seq LIMIT ?3 OFFSET ?4",
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let rows = statement
            .query_map(
                params![session_id, through, limit.max(1) as i64, start as i64],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let mut page = Vec::new();
        for row in rows {
            let (seq, raw) =
                row.map_err(|error| format!("Could not read the transcript: {error}"))?;
            let EventPayload::Message { message, .. } = decode_event(seq, &raw, 0)?.payload else {
                continue;
            };
            page.push((seq, message));
        }
        Ok((page, total.max(0) as usize))
    }

    pub fn create_session(&self, id: &str, workspace: &str, now: i64) -> Result<Session, String> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO sessions (id, workspace, title, turns, created_at, updated_at) \
             VALUES (?1, ?2, '', 0, ?3, ?3)",
            params![id, workspace, now],
        )
        .map_err(|error| format!("Could not create the session: {error}"))?;
        Self::get_with(&conn, id)?.ok_or_else(|| "The new session disappeared.".to_string())
    }

    /// History rows for one workspace. Empty tabs stay out of the palette.
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
    ) -> Result<RunStart, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not start the run: {error}"))?;
        let current_title: Option<String> = tx
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Could not read the session: {error}"))?;
        let Some(current_title) = current_title else {
            return Err("This session no longer exists.".into());
        };
        let title = if current_title.is_empty() {
            turn.title.clone().unwrap_or_default()
        } else {
            current_title
        };
        // Read the prior model surface in the same transaction that appends
        // the user turn, so a concurrent compaction cannot slip between them.
        let prior_messages = surface_messages_with(&tx, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        let start_seq = next_seq(&tx, session_id)?;
        append_events(
            &tx,
            session_id,
            &[EventPayload::Message {
                message: turn.message.clone(),
                display: turn.display.clone(),
            }],
            now,
        )?;
        tx.execute(
            "UPDATE sessions SET title = ?2, turns = turns + 1, updated_at = ?3 WHERE id = ?1",
            params![session_id, title, now],
        )
        .map_err(|error| format!("Could not save the message: {error}"))?;
        tx.execute(
            "INSERT INTO runs (id, session_id, status, started_at) VALUES (?1, ?2, 'running', ?3)",
            params![run_id, session_id, now],
        )
        .map_err(|error| format!("Could not record the run: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not start the run: {error}"))?;

        let window = Self::get_window_with(&conn, session_id, None, PAGE_EVENTS)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        Ok(RunStart {
            window,
            start_seq,
            prior_messages,
        })
    }

    /// Append the finished turn and its run status in one transaction.
    pub fn finish_run(&self, end: &RunEnd) -> Result<SessionWindow, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not finish the run: {error}"))?;
        append_events(&tx, &end.session_id, &end.appended, end.finished_at)?;
        if let Some(compaction) = &end.compaction {
            let events = decode_events(&tx, &end.session_id)?;
            let shadowed_through = shadow_for(&events, &compaction.surface);
            append_events(
                &tx,
                &end.session_id,
                &[EventPayload::Compaction {
                    summary: compaction.summary.clone(),
                    shadowed_through,
                    before: compaction.before,
                    after: compaction.after,
                }],
                end.finished_at,
            )?;
        }
        let changed = tx
            .execute(
                "UPDATE sessions SET context_used = ?2, context_window = ?3, \
                 updated_at = ?4 WHERE id = ?1",
                params![
                    end.session_id,
                    end.context.map(|context| context.used as i64),
                    end.context.map(|context| context.window as i64),
                    end.finished_at
                ],
            )
            .map_err(|error| format!("Could not save the session: {error}"))?;
        if changed == 0 {
            return Err("This session no longer exists.".into());
        }
        tx.execute(
            "UPDATE runs SET status = ?2, finished_at = ?3, error = ?4 WHERE id = ?1",
            params![end.run_id, end.status, end.finished_at, end.error],
        )
        .map_err(|error| format!("Could not update the run: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not finish the run: {error}"))?;

        let meta = Self::get_meta_with(&conn, &end.session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        let events = read_from(&conn, &end.session_id, end.start_seq)?;
        let has_more = events.first().is_some_and(|event| event.seq > 1);
        Ok(SessionWindow {
            id: meta.id,
            title: meta.title,
            turns: meta.turns,
            updated_at: meta.updated_at,
            context: meta.context,
            events,
            has_more,
        })
    }

    /// Append one compaction record and save the gauge it produced.
    pub fn apply_compaction(
        &self,
        session_id: &str,
        record: &CompactionRecord,
        now: i64,
    ) -> Result<SessionWindow, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not save the compacted context: {error}"))?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Could not read the session: {error}"))?;
        if exists.is_none() {
            return Err("This session no longer exists.".into());
        }
        append_events(
            &tx,
            session_id,
            &[EventPayload::Compaction {
                summary: record.summary.clone(),
                shadowed_through: record.shadowed_through,
                before: record.before,
                after: record.after,
            }],
            now,
        )?;
        tx.execute(
            "UPDATE sessions SET context_used = ?2, context_window = ?3, updated_at = ?4 \
             WHERE id = ?1",
            params![
                session_id,
                record.context.used as i64,
                record.context.window as i64,
                now
            ],
        )
        .map_err(|error| format!("Could not save the compacted context: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not save the compacted context: {error}"))?;

        Self::get_window_with(&conn, session_id, None, PAGE_EVENTS)?
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

        let tone = notice
            .get("tone")
            .and_then(Value::as_str)
            .unwrap_or("muted")
            .to_string();
        let text = notice
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (run_id, session_id) in &stale {
            tx.execute(
                "UPDATE runs SET status = 'interrupted', finished_at = ?2 WHERE id = ?1",
                params![run_id, now],
            )
            .map_err(|error| format!("Could not recover an old run: {error}"))?;
            append_events(
                &tx,
                session_id,
                &[EventPayload::Notice {
                    tone: tone.clone(),
                    text: text.clone(),
                }],
                now,
            )?;
            tx.execute(
                "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
                params![session_id, now],
            )
            .map_err(|error| format!("Could not recover a session: {error}"))?;
        }

        tx.commit()
            .map_err(|error| format!("Could not recover old runs: {error}"))?;
        Ok(stale.len())
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
    if version == SCHEMA_VERSION && has_table(conn, "session_events")? {
        return Ok(());
    }

    // v5 is a deliberate reset: unreleased history is discarded and the new
    // event log starts empty.
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| format!("Could not prepare the session database: {error}"))?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS session_history;
         DROP TABLE IF EXISTS session_messages;
         DROP TABLE IF EXISTS session_events;
         DROP TABLE IF EXISTS runs;
         DROP TABLE IF EXISTS sessions;",
    )
    .map_err(|error| format!("Could not reset the session database: {error}"))?;
    tx.execute_batch(SCHEMA)
        .map_err(|error| format!("Could not create the session tables: {error}"))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("Could not version the session database: {error}"))?;
    tx.commit()
        .map_err(|error| format!("Could not create the session tables: {error}"))?;
    Ok(())
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

    use super::{CompactionEnd, ContextUsage, EventPayload, NewUserTurn, RunEnd, Store, has_table};

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    fn turn(text: &str, title: Option<&str>) -> NewUserTurn {
        NewUserTurn {
            message: json!({ "role": "user", "content": text }),
            display: None,
            title: title.map(str::to_string),
        }
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
    fn begin_run_appends_one_event_and_keeps_the_first_title() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        let first = store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();
        assert_eq!(first.start_seq, 1);
        assert!(first.prior_messages.is_empty());
        assert_eq!(first.window.title, "hello");
        assert_eq!(first.window.turns, 1);
        assert_eq!(first.window.events.len(), 1);
        assert_eq!(first.window.events[0].seq, 1);
        assert!(matches!(
            first.window.events[0].payload,
            EventPayload::Message { .. }
        ));

        let second = store
            .begin_run("s1", "r2", &turn("again", Some("again")), 3)
            .unwrap();
        assert_eq!(second.start_seq, 2);
        assert_eq!(second.prior_messages.len(), 1);
        assert_eq!(second.prior_messages[0]["content"], "hello");
        assert_eq!(second.window.title, "hello");
        assert_eq!(second.window.turns, 2);
        assert_eq!(
            second
                .window
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn finish_run_appends_without_rewriting_prior_events() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();
        let saved = store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 9,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "hi" }),
                    display: Some(json!({ "runId": "r1" })),
                }],
                compaction: None,
                context: Some(ContextUsage {
                    used: 12,
                    window: 100,
                }),
                error: None,
            })
            .unwrap();

        assert_eq!(saved.events.len(), 2);
        assert_eq!(saved.events[1].seq, 2);
        assert_eq!(
            saved.context,
            Some(ContextUsage {
                used: 12,
                window: 100
            })
        );
        assert_eq!(saved.updated_at, 9);

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
    fn marks_old_running_runs_interrupted_and_appends_a_notice() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("hello", Some("hello")), 2)
            .unwrap();

        let recovered = store
            .interrupt_stale_runs(5, &json!({ "tone": "error", "text": "interrupted" }))
            .unwrap();
        assert_eq!(recovered, 1);

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.events.len(), 2);
        assert!(matches!(
            session.events[1].payload,
            EventPayload::Notice { .. }
        ));
        assert_eq!(session.updated_at, 5);

        assert!(
            store
                .interrupt_stale_runs(6, &json!({ "tone": "muted", "text": "x" }))
                .unwrap()
                == 0
        );
    }

    #[test]
    fn finish_run_records_the_compaction_shadow_from_the_retained_tail() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store
            .begin_run("s1", "r1", &turn("old", Some("old")), 2)
            .unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "old answer" }),
                    display: None,
                }],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        store.begin_run("s1", "r2", &turn("new", None), 4).unwrap();

        let summary = json!({
            "role": "user",
            "content": "<compacted-summary>old turns</compacted-summary>",
        });
        let session = store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r2".into(),
                start_seq: 3,
                status: "done",
                finished_at: 5,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "new answer" }),
                    display: None,
                }],
                compaction: Some(CompactionEnd {
                    summary: summary.clone(),
                    surface: vec![
                        summary,
                        json!({ "role": "user", "content": "new" }),
                        json!({ "role": "assistant", "content": "new answer" }),
                    ],
                    before: 100,
                    after: 10,
                }),
                context: Some(ContextUsage {
                    used: 10,
                    window: 100,
                }),
                error: None,
            })
            .unwrap();

        assert_eq!(session.events.len(), 3);
        assert_eq!(session.events[0].seq, 3);
        assert_eq!(session.events[2].seq, 5);
        assert!(session.has_more);
        assert_eq!(
            session.events.last().unwrap().payload,
            EventPayload::Compaction {
                summary: json!({
                    "role": "user",
                    "content": "<compacted-summary>old turns</compacted-summary>",
                }),
                shadowed_through: 2,
                before: 100,
                after: 10,
            }
        );
        assert_eq!(store.history_count("s1").unwrap(), 2);
        let (history, total) = store.read_history("s1", 1, 10).unwrap();
        assert_eq!(total, 2);
        assert_eq!(history[0].1["content"], "old");
        assert_eq!(history[1].1["content"], "old answer");
    }

    #[test]
    fn reads_turn_aligned_windows_backwards() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        for (index, text) in ["one", "two", "three"].into_iter().enumerate() {
            let start = index as i64 * 2 + 1;
            store
                .begin_run("s1", &format!("r{index}"), &turn(text, None), start)
                .unwrap();
            store
                .finish_run(&RunEnd {
                    session_id: "s1".into(),
                    run_id: format!("r{index}"),
                    start_seq: start,
                    status: "done",
                    finished_at: start + 1,
                    appended: vec![EventPayload::Message {
                        message: json!({ "role": "assistant", "content": text }),
                        display: None,
                    }],
                    compaction: None,
                    context: None,
                    error: None,
                })
                .unwrap();
        }

        let latest = store.get_session_window("s1", None, 2).unwrap().unwrap();
        assert_eq!(
            latest
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [5, 6]
        );
        assert!(latest.has_more);

        let middle = store.get_session_window("s1", Some(5), 2).unwrap().unwrap();
        assert_eq!(
            middle
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert!(middle.has_more);

        let oldest = store.get_session_window("s1", Some(3), 2).unwrap().unwrap();
        assert_eq!(
            oldest
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(!oldest.has_more);

        let empty = store.get_session_window("s1", Some(1), 2).unwrap().unwrap();
        assert!(empty.events.is_empty());
        assert!(!empty.has_more);
    }

    #[test]
    fn a_window_keeps_notices_above_its_first_turn() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store.begin_run("s1", "r1", &turn("old", None), 2).unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![
                    EventPayload::Message {
                        message: json!({ "role": "assistant", "content": "old answer" }),
                        display: None,
                    },
                    EventPayload::Notice {
                        tone: "muted".into(),
                        text: "interrupted".into(),
                    },
                ],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        store.begin_run("s1", "r2", &turn("new", None), 4).unwrap();

        let page = store.get_session_window("s1", Some(5), 2).unwrap().unwrap();
        assert_eq!(
            page.events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert!(matches!(
            page.events[0].payload,
            EventPayload::Notice { .. }
        ));
    }

    #[test]
    fn reads_only_the_model_surface_after_compaction() {
        let store = store();
        store.create_session("s1", "/tmp/a", 1).unwrap();
        store.begin_run("s1", "r1", &turn("old", None), 2).unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "old answer" }),
                    display: None,
                }],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        store.begin_run("s1", "r2", &turn("new", None), 4).unwrap();
        let summary = json!({
            "role": "user",
            "content": "<compacted-summary>old turns</compacted-summary>",
        });
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r2".into(),
                start_seq: 3,
                status: "done",
                finished_at: 5,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "new answer" }),
                    display: None,
                }],
                compaction: Some(CompactionEnd {
                    summary: summary.clone(),
                    surface: vec![
                        summary.clone(),
                        json!({ "role": "user", "content": "new" }),
                        json!({ "role": "assistant", "content": "new answer" }),
                    ],
                    before: 100,
                    after: 10,
                }),
                context: None,
                error: None,
            })
            .unwrap();

        {
            let conn = store.connection().unwrap();
            let messages = super::surface_messages_with(&conn, "s1").unwrap().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0], summary);
            assert_eq!(messages[1]["content"], "new");
            assert_eq!(messages[2]["content"], "new answer");
            assert!(!messages.iter().any(|message| message["content"] == "old"));
            assert!(
                super::surface_messages_with(&conn, "missing")
                    .unwrap()
                    .is_none()
            );
        }

        let later = store
            .begin_run("s1", "r3", &turn("later", None), 6)
            .unwrap();
        assert_eq!(later.start_seq, 6);
        assert_eq!(later.prior_messages.len(), 3);
        assert_eq!(later.prior_messages[0], summary);
        assert_eq!(later.prior_messages[1]["content"], "new");
    }

    #[test]
    fn resets_an_old_database_to_v5() {
        let root = crate::tools::test_support::temp_root("store-reset-v5");
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
                   messages TEXT NOT NULL DEFAULT '[]'
                 );
                 CREATE TABLE runs (
                   id TEXT PRIMARY KEY,
                   session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                   status TEXT NOT NULL,
                   started_at INTEGER NOT NULL,
                   finished_at INTEGER,
                   error TEXT
                 );
                 CREATE TABLE session_messages (
                   session_id TEXT NOT NULL,
                   seq INTEGER NOT NULL,
                   message TEXT NOT NULL,
                   PRIMARY KEY (session_id, seq)
                 );
                 CREATE TABLE session_history (
                   session_id TEXT NOT NULL,
                   seq INTEGER NOT NULL,
                   message TEXT NOT NULL,
                   archived_at INTEGER NOT NULL,
                   PRIMARY KEY (session_id, seq)
                 );
                 INSERT INTO sessions (id, workspace, created_at, updated_at)
                 VALUES ('old', '/tmp/a', 1, 1);
                 INSERT INTO runs (id, session_id, status, started_at)
                 VALUES ('r', 'old', 'done', 1);
                 INSERT INTO session_messages (session_id, seq, message)
                 VALUES ('old', 1, '{}');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert!(store.get_session("old").unwrap().is_none());
        let conn = store.connection().unwrap();
        assert!(has_table(&conn, "session_events").unwrap());
        assert!(!has_table(&conn, "session_messages").unwrap());
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        std::fs::remove_dir_all(&root).ok();
    }
}
