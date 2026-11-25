//! Exact context references. Legacy prefix records are decoded only here.
use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::events::{
    append_events, checkpoint, decode_event, is_checkpoint, next_seq, read_from, tool_trims,
};
use super::{EventPayload, Store};
use crate::context::{MessageId, identify, message_id};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceEntry {
    pub id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

fn original(conn: &Connection, session: &str, seq: i64) -> Result<Value, String> {
    let raw: String = conn
        .query_row(
            "SELECT payload FROM session_events WHERE session_id = ?1 AND seq = ?2",
            params![session, seq],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    match decode_event(seq, &raw, 0)?.payload {
        EventPayload::Message { message, display } => {
            Ok(event_message(message, display.as_ref(), seq))
        }
        _ => Err("A context reference does not name an original message.".into()),
    }
}

fn event_message(mut message: Value, display: Option<&Value>, seq: i64) -> Value {
    // Old hidden events have reliable display metadata even though their bodies have no source tag.
    if message.get("oaklet_source").is_none() && display.is_some_and(|meta| meta["hidden"] == true)
    {
        let source = if message["content"].as_array().is_some_and(|parts| {
            parts
                .iter()
                .any(|part| part["type"] == "image_url" || part["type"] == "input_image")
        }) {
            "tool_images"
        } else {
            "reminder"
        };
        message = crate::context::source(message, source);
    }
    identify(message, MessageId::Stored(seq))
}

pub(super) fn surface_messages_with(
    conn: &Connection,
    session: &str,
) -> Result<Option<Vec<Value>>, String> {
    if Store::get_meta_with(conn, session)?.is_none() {
        return Ok(None);
    }
    let latest: Option<(i64, String)> = conn.query_row(
        "SELECT seq, payload FROM session_events WHERE session_id = ?1 \
         AND json_extract(payload, '$.type') IN ('context', 'compaction') ORDER BY seq DESC LIMIT 1",
        [session], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    let mut start = 0;
    if let Some((seq, raw)) = latest {
        match decode_event(seq, &raw, 0)?.payload {
            EventPayload::Context { surface, .. } => {
                start = seq;
                for entry in surface {
                    let message = match entry.message {
                        Some(message) => identify(message, entry.id),
                        None => match entry.id {
                            MessageId::Stored(seq) => original(conn, session, seq)?,
                            _ => {
                                return Err(
                                    "A stored context contains an unresolved reference.".into()
                                );
                            }
                        },
                    };
                    messages.push(message);
                }
            }
            EventPayload::Compaction {
                summary,
                shadowed_through,
                ..
            } => {
                start = shadowed_through;
                messages = legacy_checkpoint(conn, session, seq, shadowed_through, summary)?;
            }
            _ => unreachable!(),
        }
    }
    let events = read_from(conn, session, start + 1)?;
    let trims = tool_trims(&events);
    messages.extend(events.into_iter().filter_map(|event| match event.payload {
        EventPayload::Message { message, display } => Some(event_message(
            trims.get(&event.seq).cloned().unwrap_or(message),
            display.as_ref(),
            event.seq,
        )),
        _ => None,
    }));
    Ok(Some(messages))
}

fn legacy_checkpoint(
    conn: &Connection,
    session: &str,
    seq: i64,
    through: i64,
    summary: Value,
) -> Result<Vec<Value>, String> {
    const NOTICE: &str = "Earlier messages replaced by this checkpoint are archived; call history_read if you need their exact commands, errors, or file contents.";
    let mut checkpoint = checkpoint(summary);
    let text = checkpoint["content"]
        .as_str()
        .or_else(|| checkpoint["content"][0]["text"].as_str())
        .unwrap_or("");
    let end = format!("</compacted-summary>\n\n{NOTICE}");
    let split = text
        .strip_prefix("<compacted-summary>")
        .and_then(|rest| {
            rest.split_once(&end)
                .or_else(|| rest.split_once("</compacted-summary>"))
        })
        .map(|(summary, request)| {
            (
                summary.to_string(),
                request.trim_start_matches('\n').to_string(),
            )
        });
    let mut request = None;
    if let Some((summary, plain)) = split {
        let mut recovered = crate::context::provider_message(checkpoint.clone());
        match recovered.get_mut("content") {
            Some(Value::String(text)) => *text = plain,
            Some(Value::Array(parts)) => {
                if plain.is_empty() {
                    parts.remove(0);
                } else {
                    parts[0]["text"] = json!(plain);
                }
            }
            _ => {}
        }
        let has_request = match &recovered["content"] {
            Value::String(text) => !text.is_empty(),
            Value::Array(parts) => !parts.is_empty(),
            _ => false,
        };
        if has_request {
            let original = original(conn, session, through)?;
            if original["role"] != "user"
                || crate::context::provider_message(original.clone()) != recovered
            {
                return Err(
                    "The legacy checkpoint's user request cannot be recovered safely.".into(),
                );
            }
            request = Some(original);
        }
        checkpoint["content"] = json!(format!("<compacted-summary>{summary}</compacted-summary>"));
    }
    let mut messages = vec![identify(checkpoint, MessageId::Stored(seq))];
    messages.extend(request);
    Ok(messages)
}

fn resolve(id: MessageId, pending_start: i64, checkpoint_seq: i64) -> MessageId {
    match id {
        MessageId::Pending(index) => MessageId::Stored(pending_start + index as i64),
        MessageId::Checkpoint => MessageId::Stored(checkpoint_seq),
        other => other,
    }
}

pub(super) fn pending_archive(
    conn: &Connection,
    session: &str,
    appended: &[EventPayload],
    surface: &[Value],
) -> Result<Vec<(i64, Value)>, String> {
    let mut base = surface_messages_with(conn, session)?.unwrap_or_default();
    base.extend(
        appended
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                EventPayload::Message { message, .. } => {
                    Some(identify(message.clone(), MessageId::Pending(index)))
                }
                _ => None,
            }),
    );
    let visible = surface
        .iter()
        .filter(|message| message["role"] != "system")
        .map(|message| Ok((message_id(message)?, message)))
        .collect::<Result<HashMap<_, _>, String>>()?;
    let pending_start = next_seq(conn, session)?;
    let mut archived = Vec::new();
    for message in base {
        if is_checkpoint(&message) {
            continue;
        }
        let id = message_id(&message)?;
        if visible.get(&id).is_some_and(|retained| {
            message["role"] != "tool" || retained["content"] == message["content"]
        }) {
            continue;
        }
        let (seq, original) = match id {
            MessageId::Stored(seq) => (seq, original(conn, session, seq)?),
            MessageId::Pending(index) => (pending_start + index as i64, message),
            MessageId::Repair(_) => continue,
            MessageId::Checkpoint => unreachable!(),
        };
        archived.push((seq, crate::context::provider_message(original)));
    }
    archived.sort_by_key(|(seq, _)| *seq);
    Ok(archived)
}

pub(super) fn save_surface(
    conn: &Connection,
    session: &str,
    appended: &[EventPayload],
    surface: &[Value],
    before: usize,
    after: usize,
    now: i64,
) -> Result<(), String> {
    let archived = pending_archive(conn, session, appended, surface)?;
    let pending_start = next_seq(conn, session)?;
    let checkpoint_seq = pending_start + appended.len() as i64;
    let mut known = surface_messages_with(conn, session)?.unwrap_or_default();
    known.extend(
        appended
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                EventPayload::Message { message, .. } => {
                    Some(identify(message.clone(), MessageId::Pending(index)))
                }
                _ => None,
            }),
    );
    let known = known
        .into_iter()
        .map(|message| Ok((message_id(&message)?, message)))
        .collect::<Result<HashMap<_, _>, String>>()?;
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for message in surface {
        let id = message_id(message)?;
        if !seen.insert(id.clone()) {
            return Err("Duplicate context identity; nothing was replaced.".into());
        }
        let replacement = if id == MessageId::Checkpoint {
            if !is_checkpoint(message) {
                return Err("Invalid checkpoint identity.".into());
            }
            Some(message.clone())
        } else {
            if let MessageId::Repair(call) = &id {
                if message["oaklet_source"] != "repair"
                    || message["role"] != "tool"
                    || message["tool_call_id"] != *call
                {
                    return Err("Invalid repaired tool result.".into());
                }
            } else {
                let prior = known
                    .get(&id)
                    .ok_or("The conversation changed; context identity is no longer present.")?;
                let mut allowed = prior.clone();
                if prior["role"] == "tool" && message["role"] == "tool" {
                    allowed["content"] = message["content"].clone();
                }
                if allowed != *message {
                    return Err(
                        "A retained conversation message was changed; nothing was replaced.".into(),
                    );
                }
            }
            match id {
                MessageId::Stored(seq) => {
                    let raw = original(conn, session, seq);
                    if raw.as_ref().is_ok_and(|raw| raw == message) {
                        None
                    } else {
                        Some(message.clone())
                    }
                }
                MessageId::Pending(_) if known.get(&id) == Some(message) => None,
                _ => Some(message.clone()),
            }
        };
        let id = resolve(id, pending_start, checkpoint_seq);
        entries.push(SurfaceEntry {
            id: id.clone(),
            message: replacement.map(|message| identify(message, id)),
        });
    }
    append_events(conn, session, appended, now)?;
    let archived: Vec<i64> = archived.into_iter().map(|(seq, _)| seq).collect();
    append_events(
        conn,
        session,
        &[EventPayload::Context {
            surface: entries,
            archived: archived.clone(),
            before,
            after,
        }],
        now,
    )?;
    for seq in archived {
        conn.execute(
            "INSERT OR IGNORE INTO archived_messages (session_id, event_seq) VALUES (?1, ?2)",
            params![session, seq],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn index_legacy_archive(
    conn: &Connection,
    session: &str,
    through: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO archived_messages SELECT session_id, seq FROM session_events \
        WHERE session_id = ?1 AND seq <= ?2 AND json_extract(payload, '$.type') = 'message'",
        params![session, through],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CompactionEnd, CompactionRecord, ContextUsage, NewUserTurn, RunEnd};

    fn message(message: Value) -> EventPayload {
        EventPayload::Message {
            message,
            display: None,
        }
    }
    fn user(text: &str) -> Value {
        json!({"role":"user", "content":text})
    }
    fn end(appended: Vec<EventPayload>, surface: Vec<Value>) -> RunEnd {
        RunEnd {
            session_id: "s".into(),
            run_id: "r".into(),
            start_seq: 1,
            status: "done",
            finished_at: 3,
            appended,
            compaction: Some(CompactionEnd {
                surface,
                before: 5000,
                after: 1000,
            }),
            context: None,
            usage: Default::default(),
            error: None,
        }
    }
    fn checkpoint_message() -> Value {
        identify(
            checkpoint(user("<compacted-summary>Goal: finish</compacted-summary>")),
            MessageId::Checkpoint,
        )
    }

    #[test]
    fn pending_identities_and_trimmed_originals_survive_reopen_and_repeated_replacement() {
        let root = crate::tools::test_support::temp_root("context-projection");
        let path = root.join("oaklet.db");
        let store = Store::open(&path).unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        let request = user("Never commit. <compacted-summary>quoted text</compacted-summary>");
        store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: request.clone(),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let call = json!({"role":"assistant", "tool_calls":[{"id":"c","type":"function","function":{"name":"read_file","arguments":"{}"}}]});
        let original =
            json!({"role":"tool", "tool_call_id":"c", "content":"original output ".repeat(1000)});
        let appended = vec![message(call.clone()), message(original.clone())];
        let mut trimmed = identify(original.clone(), MessageId::Pending(1));
        trimmed["content"] = json!("trimmed output");
        let surface = vec![
            checkpoint_message(),
            identify(request.clone(), MessageId::Stored(1)),
            identify(call, MessageId::Pending(0)),
            trimmed,
        ];
        let pending = store.pending_archive("s", &appended, &surface).unwrap();
        assert_eq!(pending, vec![(3, original.clone())]);
        let end = end(appended, surface);
        store.finish_run(&end).unwrap();
        store.finish_run(&end).unwrap();
        let (first, version) = store.context_snapshot("s").unwrap();
        assert_eq!(first[1], identify(request, MessageId::Stored(1)));
        assert_eq!(message_id(&first[2]).unwrap(), MessageId::Stored(2));
        assert_eq!(message_id(&first[3]).unwrap(), MessageId::Stored(3));
        let mut second = first.clone();
        second[0] = checkpoint_message();
        store
            .apply_surface(
                "s",
                version,
                &second,
                (1000, 900),
                ContextUsage {
                    used: 900,
                    window: 8192,
                    segments: Vec::new(),
                },
                4,
            )
            .unwrap();
        let (expected, _) = store.context_snapshot("s").unwrap();
        assert_eq!(&expected[1..], &first[1..]);
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.context_snapshot("s").unwrap().0, expected);
        assert_eq!(
            reopened.read_history("s", 1, 8).unwrap(),
            (vec![(3, original)], 1)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_identity_or_version_validation_writes_nothing() {
        let store = Store::in_memory().unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: user("original"),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let (base, version) = store.context_snapshot("s").unwrap();
        let context = ContextUsage {
            used: 10,
            window: 8192,
            segments: Vec::new(),
        };
        let mut modified = base.clone();
        modified[0]["content"] = json!("changed");
        assert!(
            store
                .apply_surface("s", version, &modified, (20, 10), context.clone(), 3)
                .is_err()
        );
        let mut missing = base.clone();
        missing[0] = identify(missing[0].clone(), MessageId::Stored(999));
        assert!(
            store
                .apply_surface("s", version, &missing, (20, 10), context.clone(), 3)
                .is_err()
        );
        assert!(
            store
                .apply_surface("s", version - 1, &base, (20, 10), context, 3)
                .is_err()
        );
        assert_eq!(
            store.context_snapshot("s").unwrap(),
            (base.clone(), version)
        );
        let bad = end(
            vec![message(json!({"role":"assistant", "content":"new"}))],
            modified,
        );
        assert!(store.finish_run(&bad).is_err());
        assert_eq!(store.context_snapshot("s").unwrap(), (base, version));
    }

    #[test]
    fn duplicate_texts_are_distinguished_by_identity_and_pending_pagination_stays_stable() {
        let store = Store::in_memory().unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: user("same"),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let appended = vec![
            message(user("same")),
            message(json!({"role":"assistant","content":"done"})),
        ];
        let surface = vec![
            checkpoint_message(),
            identify(user("same"), MessageId::Pending(0)),
            identify(
                json!({"role":"assistant","content":"done"}),
                MessageId::Pending(1),
            ),
        ];
        let pending = store.pending_archive("s", &appended, &surface).unwrap();
        let before = store.read_history_pending("s", 1, 8, &pending).unwrap();
        store.finish_run(&end(appended, surface)).unwrap();
        assert_eq!(store.read_history("s", 1, 8).unwrap(), before);
        assert_eq!(before, (vec![(1, user("same"))], 1));
    }

    #[test]
    fn legacy_merged_request_is_split_once_at_the_storage_boundary() {
        let store = Store::in_memory().unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: user("Never deploy"),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        store
            .apply_compaction(
                "s",
                &CompactionRecord {
                    summary: user(
                        "<compacted-summary>old facts</compacted-summary>\n\nNever deploy",
                    ),
                    shadowed_through: 1,
                    before: 100,
                    after: 10,
                    context: ContextUsage {
                        used: 10,
                        window: 8192,
                        segments: Vec::new(),
                    },
                },
                3,
            )
            .unwrap();
        let (messages, version) = store.context_snapshot("s").unwrap();
        assert_eq!(messages.len(), 2);
        assert!(is_checkpoint(&messages[0]));
        assert_eq!(
            messages[1],
            identify(user("Never deploy"), MessageId::Stored(1))
        );
        store
            .apply_surface(
                "s",
                version,
                &messages,
                (100, 10),
                ContextUsage {
                    used: 10,
                    window: 8192,
                    segments: Vec::new(),
                },
                4,
            )
            .unwrap();
        assert_eq!(store.context_snapshot("s").unwrap().0, messages);
    }
}

#[cfg(test)]
mod legacy_source_tests {
    use super::*;

    #[test]
    fn old_hidden_sources_use_display_metadata_and_user_text_stays_a_request() {
        let text = json!({"role":"user","content":"Oaklet loop guard: fix this diagnostic"});
        assert!(crate::context::is_request(&event_message(
            text.clone(),
            None,
            1
        )));
        let hidden = event_message(text, Some(&json!({"hidden":true})), 2);
        assert!(!crate::context::is_request(&hidden));
        let image = event_message(
            json!({"role":"user","content":[
                {"type":"text","text":"Images returned by the previous tool call(s)."},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}
            ]}),
            Some(&json!({"hidden":true})),
            3,
        );
        assert_eq!(image["oaklet_source"], "tool_images");
    }
}
