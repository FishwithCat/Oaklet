//! Archived originals, exact context surfaces, and compaction writes.

use std::collections::HashMap;

use rusqlite::params;
#[cfg(test)]
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

#[cfg(test)]
use super::CompactionRecord;
use super::events::next_seq;
#[cfg(test)]
use super::events::{append_events, checkpoint, decode_event};
use super::projection::{self, save_surface, surface_messages_with};
use super::{ContextUsage, EventPayload, PAGE_EVENTS, SessionWindow, Store};

#[cfg(test)]
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
        } => Ok(Some((checkpoint(summary), shadowed_through))),
        _ => Ok(None),
    }
}

impl Store {
    /// Read the surface and its version under one database lock.
    pub fn context_snapshot(&self, session_id: &str) -> Result<(Vec<Value>, i64), String> {
        let conn = self.connection()?;
        let messages = surface_messages_with(&conn, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        Ok((messages, next_seq(&conn, session_id)? - 1))
    }

    pub fn has_history(&self, session_id: &str) -> Result<bool, String> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM archived_messages WHERE session_id = ?1)",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())
    }

    pub fn pending_archive(
        &self,
        session_id: &str,
        appended: &[EventPayload],
        surface: &[Value],
    ) -> Result<Vec<(i64, Value)>, String> {
        let conn = self.connection()?;
        projection::pending_archive(&conn, session_id, appended, surface)
    }

    pub fn apply_surface(
        &self,
        session_id: &str,
        version: i64,
        surface: &[Value],
        sizes: (usize, usize),
        context: ContextUsage,
        now: i64,
    ) -> Result<SessionWindow, String> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        if next_seq(&tx, session_id)? - 1 != version {
            return Err("The conversation changed during compaction; nothing was replaced.".into());
        }
        save_surface(&tx, session_id, &[], surface, sizes.0, sizes.1, now)?;
        tx.execute(
            "UPDATE sessions SET context_used = ?2, context_window = ?3, \
                    context_segments = ?4, updated_at = ?5 WHERE id = ?1",
            params![
                session_id,
                context.used as i64,
                context.window as i64,
                context.segments_json()?,
                now
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Self::get_window_with(&conn, session_id, None, PAGE_EVENTS)?
            .ok_or_else(|| "This session no longer exists.".into())
    }

    #[cfg(test)]
    pub fn history_count(&self, session_id: &str) -> Result<usize, String> {
        self.read_history(session_id, 1, 1).map(|(_, total)| total)
    }

    #[cfg(test)]
    pub fn read_history(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<(i64, Value)>, usize), String> {
        self.read_history_pending(session_id, offset, limit, &[])
    }

    /// Merge live and durable originals by event sequence before paging.
    pub fn read_history_pending(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
        pending: &[(i64, Value)],
    ) -> Result<(Vec<(i64, Value)>, usize), String> {
        const ARCHIVE: &str = "WITH stored AS (\
            SELECT event_seq AS seq FROM archived_messages WHERE session_id = ?1), \
            archived AS (SELECT seq FROM stored UNION ALL \
                SELECT value AS seq FROM json_each(?2) WHERE value NOT IN (SELECT seq FROM stored)) ";
        let conn = self.connection()?;
        let pending: HashMap<_, _> = pending
            .iter()
            .map(|(seq, message)| (*seq, message))
            .collect();
        let pending_ids = serde_json::to_string(&pending.keys().copied().collect::<Vec<_>>())
            .map_err(|error| error.to_string())?;
        let total: i64 = conn
            .query_row(
                &format!("{ARCHIVE} SELECT COUNT(*) FROM archived"),
                params![session_id, pending_ids],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let mut statement = conn
            .prepare(&format!(
                "{ARCHIVE} SELECT a.seq, json_extract(e.payload, '$.message') FROM archived a \
                 LEFT JOIN session_events e ON e.session_id = ?1 AND e.seq = a.seq ORDER BY a.seq LIMIT ?3 OFFSET ?4"
            ))
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let rows = statement
            .query_map(
                params![
                    session_id,
                    pending_ids,
                    limit.max(1) as i64,
                    (offset.max(1) - 1) as i64
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(|error| format!("Could not read the transcript: {error}"))?;
        let mut page = Vec::new();
        for row in rows {
            let (seq, raw) =
                row.map_err(|error| format!("Could not read the transcript: {error}"))?;
            page.push((
                seq,
                match raw {
                    Some(raw) => serde_json::from_str(&raw)
                        .map_err(|error| format!("Could not read the transcript: {error}"))?,
                    None => (*pending
                        .get(&seq)
                        .ok_or("An archived message is unavailable.")?)
                    .clone(),
                },
            ));
        }
        Ok((page, total.max(0) as usize))
    }

    /// Seed legacy records in compatibility tests only.
    #[cfg(test)]
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
        let previous = latest_compaction(&tx, session_id)?
            .map(|(_, seq)| seq)
            .unwrap_or(0);
        let next = next_seq(&tx, session_id)?;
        if record.shadowed_through < previous || record.shadowed_through >= next {
            return Err("Invalid compaction boundary; nothing was compacted.".into());
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
        projection::index_legacy_archive(&tx, session_id, record.shadowed_through)?;
        tx.execute(
            "UPDATE sessions SET context_used = ?2, context_window = ?3, \
             context_segments = ?4, updated_at = ?5 \
             WHERE id = ?1",
            params![
                session_id,
                record.context.used as i64,
                record.context.window as i64,
                record.context.segments_json()?,
                now
            ],
        )
        .map_err(|error| format!("Could not save the compacted context: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Could not save the compacted context: {error}"))?;

        Self::get_window_with(&conn, session_id, None, PAGE_EVENTS)?
            .ok_or_else(|| "This session no longer exists.".to_string())
    }
}
