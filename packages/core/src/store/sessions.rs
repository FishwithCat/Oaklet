//! Session rows, metadata, and UI windows.

use rusqlite::{Connection, OptionalExtension, params};

use super::events::{decode_events, latest_plan, plan_state, read_last_step, read_page};
use super::{
    ContextUsage, PlanRecord, PlanState, Session, SessionMeta, SessionSummary, SessionWindow, Store,
};

const SESSION_COLUMNS: &str = "id, workspace, title, turns, created_at, updated_at, \
     context_used, context_window";

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
        workspace: meta.workspace,
        title: meta.title,
        turns: meta.turns,
        updated_at: meta.updated_at,
        context: meta.context,
        events,
    })
}

impl Store {
    pub(super) fn get_with(conn: &Connection, id: &str) -> Result<Option<Session>, String> {
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

    pub(super) fn get_meta_with(
        conn: &Connection,
        id: &str,
    ) -> Result<Option<SessionMeta>, String> {
        conn.query_row(
            &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
            [id],
            read_session_row,
        )
        .optional()
        .map(|row| row.map(to_meta))
        .map_err(|error| format!("Could not read the session: {error}"))
    }

    pub(super) fn get_window_with(
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
        let last_usage = read_last_step(conn, id)?;
        Ok(Some(SessionWindow {
            id: meta.id,
            workspace: meta.workspace,
            title: meta.title,
            turns: meta.turns,
            updated_at: meta.updated_at,
            context: meta.context,
            last_usage,
            events,
            has_more,
        }))
    }

    #[cfg(test)]
    pub fn get_session(&self, id: &str) -> Result<Option<Session>, String> {
        let conn = self.connection()?;
        Self::get_with(&conn, id)
    }

    /// Metadata for one session, without reading its event log.
    pub fn session_meta(&self, id: &str) -> Result<Option<SessionMeta>, String> {
        let conn = self.connection()?;
        Self::get_meta_with(&conn, id)
    }

    /// The newest plan version and whether it still needs review.
    pub fn plan_state(&self, id: &str) -> Result<PlanState, String> {
        let conn = self.connection()?;
        plan_state(&conn, id)
    }

    /// The newest plan payload, used by the confirmation command.
    pub fn latest_plan(&self, id: &str) -> Result<Option<PlanRecord>, String> {
        let conn = self.connection()?;
        latest_plan(&conn, id)
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
}
