//! Reading, paging, and appending events in the transcript log.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::{Event, EventPayload, PlanRecord, PlanState, PlanStatus, StepUsage};

pub(super) fn decode_event(seq: i64, raw: &str, created_at: i64) -> Result<Event, String> {
    let payload = serde_json::from_str(raw)
        .map_err(|_| "A transcript event could not be read.".to_string())?;
    Ok(Event {
        seq,
        payload,
        created_at,
    })
}

pub(super) fn read_events(
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

pub(super) fn decode_events(conn: &Connection, session_id: &str) -> Result<Vec<Event>, String> {
    read_events(
        conn,
        "SELECT seq, payload, created_at FROM session_events \
         WHERE session_id = ?1 ORDER BY seq",
        &[&session_id],
    )
}

/// A page is cut before it is aligned, so a turn with more than one page still
/// starts where the page starts; every smaller turn starts on its user message.
pub(super) fn align_page(events: &mut Vec<Event>) {
    let Some(index) = events.iter().position(is_visible_user_event) else {
        return;
    };
    // Keep notices directly above the turn (compaction markers, interruption
    // errors); drop a partial assistant/tool block instead.
    let mut start = index;
    while start > 0
        && matches!(
            events[start - 1].payload,
            EventPayload::Notice { .. }
                | EventPayload::Compaction { .. }
                | EventPayload::Context { .. }
                | EventPayload::Plan { .. }
        )
    {
        start -= 1;
    }
    if start > 0 {
        events.drain(0..start);
    }
}

pub(super) fn is_visible_user_event(event: &Event) -> bool {
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

pub(super) fn read_page(
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

pub(super) fn read_from(
    conn: &Connection,
    session_id: &str,
    start_seq: i64,
) -> Result<Vec<Event>, String> {
    read_events(
        conn,
        "SELECT seq, payload, created_at FROM session_events \
         WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq",
        &[&session_id, &start_seq],
    )
}

pub(super) fn next_seq(conn: &Connection, session_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_events WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .map_err(|error| format!("Could not read the transcript: {error}"))
}

/// Last completed run's newest request; a running run has no counts yet.
pub(super) fn read_last_step(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<StepUsage>, String> {
    conn.query_row(
        "SELECT last_prompt_tokens, last_completion_tokens, last_cache_hit_tokens, \
         last_cache_miss_tokens FROM runs WHERE session_id = ?1 AND model_steps > 0 \
         AND last_prompt_tokens > 0 ORDER BY started_at DESC, rowid DESC LIMIT 1",
        [session_id],
        |row| {
            Ok(StepUsage {
                prompt_tokens: row.get::<_, i64>(0)?.max(0) as usize,
                completion_tokens: row.get::<_, i64>(1)?.max(0) as usize,
                cache_hit_tokens: row.get::<_, i64>(2)?.max(0) as usize,
                cache_miss_tokens: row.get::<_, i64>(3)?.max(0) as usize,
            })
        },
    )
    .optional()
    .map_err(|error| format!("Could not read run usage: {error}"))
}

/// The newest plan version with the payload confirmation needs.
pub(super) fn latest_plan(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<PlanRecord>, String> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT payload FROM session_events WHERE session_id = ?1 \
             AND json_extract(payload, '$.type') = 'plan' ORDER BY seq DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not read the plan: {error}"))?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    match decode_event(0, &raw, 0)?.payload {
        EventPayload::Plan {
            version,
            status,
            steps,
            validation,
        } => Ok(Some(PlanRecord {
            version,
            status,
            steps,
            validation: validation.map(|validation| *validation),
        })),
        _ => Ok(None),
    }
}

/// The newest plan and whether it is still awaiting review.
pub(super) fn plan_state(conn: &Connection, session_id: &str) -> Result<PlanState, String> {
    Ok(
        latest_plan(conn, session_id)?.map_or_else(PlanState::default, |plan| PlanState {
            latest: Some(plan.version),
            pending: plan.status == PlanStatus::Pending,
        }),
    )
}

pub(super) fn append_events(
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

pub(crate) fn tool_trims(events: &[Event]) -> HashMap<i64, Value> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ToolTrim {
                original_seq,
                message,
            } => Some((*original_seq, message.clone())),
            _ => None,
        })
        .collect()
}

/// A compaction checkpoint is user-visible history, not conversation.
pub(crate) fn is_checkpoint(message: &Value) -> bool {
    message["role"] == "user" && message["oaklet_checkpoint"] == true
}

pub(crate) fn checkpoint(mut message: Value) -> Value {
    message["oaklet_checkpoint"] = Value::Bool(true);
    message
}
