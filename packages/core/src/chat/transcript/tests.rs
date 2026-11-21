use super::*;
use crate::chat::{ChatState, run::trim_incomplete_tool_block};

/// Model-facing history: the latest summary plus every message after its
/// boundary. Display-only data never crosses this line.
fn surface_messages(events: &[Event]) -> Vec<Value> {
    let through = events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::Compaction {
            shadowed_through, ..
        } => Some(*shadowed_through),
        _ => None,
    });
    let start = through.unwrap_or(0);
    let mut messages = Vec::new();
    if through.is_some()
        && let Some(EventPayload::Compaction { summary, .. }) = events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, EventPayload::Compaction { .. }))
            .map(|event| &event.payload)
    {
        messages.push(store::checkpoint(summary.clone()));
    }
    let trims = store::tool_trims(events);
    messages.extend(events.iter().filter_map(|event| match &event.payload {
        EventPayload::Message { message, .. } if event.seq > start => {
            Some(trims.get(&event.seq).unwrap_or(message).clone())
        }
        _ => None,
    }));
    trim_incomplete_tool_block(messages)
}

#[test]
fn serializes_views_and_items_in_the_frontend_shape() {
    let view = SessionView {
        id: "s1".into(),
        workspace: "/tmp/ws".into(),
        title: "chat".into(),
        updated_at: 7,
        turns: 2,
        items: Vec::new(),
        first_seq: None,
        has_more: false,
        context: None,
        last_usage: None,
        running: false,
        blocked: false,
        compacting: false,
        active_run_id: None,
    };
    let value = serde_json::to_value(&view).unwrap();
    assert_eq!(value["workspace"], "/tmp/ws");
    assert_eq!(value["updatedAt"], 7);
    assert_eq!(value["activeRunId"], serde_json::Value::Null);
    assert_eq!(value["context"], serde_json::Value::Null);
    assert_eq!(value["lastUsage"], serde_json::Value::Null);
    assert_eq!(value["firstSeq"], serde_json::Value::Null);
    assert_eq!(value["hasMore"], false);

    let item = serde_json::to_value(NewItem::Tool(Box::new(ToolItem {
        run_id: "r1".into(),
        id: "t1".into(),
        name: "read_file".into(),
        status: ToolStatus::Pending,
        summary: "a.ts".into(),
        output: None,
        started_at: Some(5),
        duration_ms: None,
        request_id: None,
        approval_prompt: None,
        approval_detail: None,
        diffs: Vec::new(),
    })))
    .unwrap();
    assert_eq!(item["kind"], "tool");
    assert_eq!(item["runId"], "r1");
    assert_eq!(item["startedAt"], 5);
    assert!(item.get("output").is_none());
}

#[test]
fn formats_tokens_like_the_composer() {
    assert_eq!(format_tokens(900), "900");
    assert_eq!(format_tokens(12_288), "12k");
    assert_eq!(format_tokens(1_536), "1.5k");
}

#[test]
fn merges_streams_and_settles_on_a_kind_boundary() {
    let mut items = Vec::new();
    merge_stream(&mut items, "r1", "reasoning", "think");
    let started_at = items[0]["startedAt"].clone();
    merge_stream(&mut items, "r1", "reasoning", "ing");
    assert_eq!(items[0]["startedAt"], started_at);
    merge_stream(&mut items, "r1", "assistant", "hello");
    merge_stream(&mut items, "r1", "assistant", " world");
    merge_stream(&mut items, "r1", "reasoning", "again");

    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["text"], "thinking");
    assert_eq!(items[0]["streaming"], false);
    assert!(items[0]["durationMs"].as_u64().is_some());
    assert!(items[0].get("startedAt").is_none());
    assert_eq!(items[1]["text"], "hello world");
    assert_eq!(items[1]["streaming"], false);
    assert_eq!(items[2]["streaming"], true);
    assert!(items[2]["startedAt"].as_u64().is_some());
}

#[test]
fn settles_unfinished_tools_at_the_end_of_a_run() {
    let mut items = vec![
        json!({
            "kind": "tool",
            "id": "a",
            "status": "awaiting",
            "requestId": "1",
            "diffs": [{ "path": "a.txt", "oldText": null, "newText": "one" }],
        }),
        json!({ "kind": "tool", "id": "b", "status": "ok", "output": "done" }),
        json!({ "kind": "assistant", "runId": "r", "text": "hi", "streaming": true }),
    ];
    settle_streaming(&mut items);
    settle_unfinished_tools(&mut items);

    assert_eq!(items[0]["status"], "error");
    assert!(items[0].get("requestId").is_none());
    assert!(items[0].get("diffs").is_none());
    assert_eq!(items[1]["status"], "ok");
    assert_eq!(items[2]["streaming"], false);
    assert!(!is_blocked(&items));
}

#[test]
fn projects_events_into_items_without_leaking_display_data() {
    let events = vec![
        Event {
            seq: 1,
            created_at: 1,
            payload: EventPayload::Message {
                message: json!({
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "look" },
                        { "type": "image_url", "image_url": { "url": "data:full" } },
                    ],
                }),
                display: Some(json!({
                    "text": "look",
                    "attachments": [{
                        "kind": "image",
                        "name": "shot.png",
                        "bytes": 10,
                        "thumbnail": "data:thumb",
                    }],
                })),
            },
        },
        Event {
            seq: 2,
            created_at: 2,
            payload: EventPayload::Message {
                message: json!({
                    "role": "assistant",
                    "content": "done",
                    "reasoning_content": "thinking",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{}" },
                    }],
                }),
                display: Some(json!({ "runId": "r1", "reasoningDurationMs": 1250 })),
            },
        },
        Event {
            seq: 3,
            created_at: 3,
            payload: EventPayload::Message {
                message: json!({ "role": "tool", "tool_call_id": "c1", "content": "file body" }),
                display: Some(json!({
                    "runId": "r1",
                    "name": "read_file",
                    "summary": "a.rs",
                    "ok": false,
                    "diffs": [{ "path": "a.rs", "oldText": null, "newText": "x" }],
                })),
            },
        },
        Event {
            seq: 4,
            created_at: 4,
            payload: EventPayload::Notice {
                tone: "muted".into(),
                text: "Stopped.".into(),
            },
        },
        Event {
            seq: 5,
            created_at: 5,
            payload: EventPayload::Message {
                message: json!({ "role": "user", "content": "hidden" }),
                display: Some(json!({ "hidden": true })),
            },
        },
    ];

    let items = project_events(&events);
    assert_eq!(items.len(), 5);
    assert_eq!(items[0]["kind"], "user");
    assert_eq!(items[0]["text"], "look");
    assert_eq!(items[0]["attachments"][0]["thumbnail"], "data:thumb");
    assert_eq!(items[1]["kind"], "reasoning");
    assert_eq!(items[1]["durationMs"], 1250);
    assert_eq!(items[2]["kind"], "assistant");
    assert_eq!(items[2]["runId"], "r1");
    assert_eq!(items[3]["kind"], "tool");
    assert_eq!(items[3]["status"], "error");
    assert_eq!(items[3]["summary"], "a.rs");
    assert_eq!(items[3]["diffs"][0]["path"], "a.rs");
    assert_eq!(items[4]["kind"], "notice");

    let model = surface_messages(&events);
    assert_eq!(model.len(), 4);
    assert!(model.iter().all(|message| message.get("oaklet").is_none()));
}

#[test]
fn a_user_message_that_quotes_a_checkpoint_marker_stays_visible() {
    let events = vec![Event {
        seq: 1,
        created_at: 1,
        payload: EventPayload::Message {
            message: json!({
                "role": "user",
                "content": "the log shows <compacted-summary> and then stops",
            }),
            display: None,
        },
    }];
    let items = project_events(&events);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "user");
    assert_eq!(
        items[0]["text"],
        "the log shows <compacted-summary> and then stops"
    );
}

#[test]
fn compaction_notice_names_the_token_drop_without_the_history_hint() {
    let events = vec![
        Event {
            seq: 1,
            created_at: 1,
            payload: EventPayload::Compaction {
                summary: json!({ "role": "user", "content": "summary" }),
                shadowed_through: 0,
                before: 81_000,
                after: 13_000,
            },
        },
        Event {
            seq: 2,
            created_at: 2,
            payload: EventPayload::Context {
                surface: Vec::new(),
                archived: Vec::new(),
                before: 81_000,
                after: 13_000,
            },
        },
    ];
    let items = project_events(&events);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["kind"], "notice");
    assert_eq!(items[0]["text"], "Compacted context: 79k → 13k tokens.");
    assert_eq!(items[1]["text"], "Compacted context: 79k → 13k tokens.");
}

#[test]
fn a_finished_run_projects_back_to_the_same_rows() {
    let state = ChatState::in_memory();
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let message = json!({ "role": "user", "content": "hi" });
    state
        .store
        .begin_run(
            "s1",
            "r1",
            &store::NewUserTurn {
                message: message.clone(),
                display: Some(json!({ "text": "hi" })),
                title: Some("hi".into()),
            },
            2,
        )
        .unwrap();
    let saved = state
        .store
        .finish_run(&store::RunEnd {
            session_id: "s1".into(),
            run_id: "r1".into(),
            start_seq: 1,
            status: "done",
            finished_at: 3,
            appended: vec![
                EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "hello" }),
                    display: Some(json!({ "runId": "r1" })),
                },
                EventPayload::Message {
                    message: json!({
                        "role": "tool",
                        "tool_call_id": "c1",
                        "content": "output",
                    }),
                    display: Some(json!({
                        "runId": "r1",
                        "name": "run_command",
                        "summary": "ls",
                        "ok": true,
                        "durationMs": 1250,
                    })),
                },
            ],
            compaction: None,
            context: Some(store::ContextUsage {
                used: 10,
                window: 100,
            }),
            usage: Default::default(),
            error: None,
        })
        .unwrap();

    let items = project_events(&saved.events);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["kind"], "user");
    assert_eq!(items[1]["kind"], "assistant");
    assert_eq!(items[2]["kind"], "tool");
    assert_eq!(items[2]["status"], "ok");
    assert_eq!(items[2]["output"], "output");
    assert_eq!(items[2]["durationMs"], 1250);
    assert!(
        surface_messages(&saved.events)[2]
            .get("durationMs")
            .is_none()
    );
}

#[test]
fn a_second_turn_rebuilds_the_model_history_from_the_events() {
    let state = ChatState::in_memory();
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let first = json!({ "role": "user", "content": "one" });
    state
        .store
        .begin_run(
            "s1",
            "r1",
            &store::NewUserTurn {
                message: first,
                display: Some(json!({ "text": "one" })),
                title: Some("one".into()),
            },
            2,
        )
        .unwrap();
    state
        .store
        .finish_run(&store::RunEnd {
            session_id: "s1".into(),
            run_id: "r1".into(),
            start_seq: 1,
            status: "done",
            finished_at: 3,
            appended: vec![EventPayload::Message {
                message: json!({ "role": "assistant", "content": "first" }),
                display: None,
            }],
            compaction: None,
            context: None,
            usage: Default::default(),
            error: None,
        })
        .unwrap();

    let second = json!({ "role": "user", "content": "two" });
    let session = state
        .store
        .begin_run(
            "s1",
            "r2",
            &store::NewUserTurn {
                message: second,
                display: Some(json!({ "text": "two" })),
                title: None,
            },
            4,
        )
        .unwrap();

    let model = surface_messages(&session.window.events);
    assert_eq!(model.len(), 3);
    assert_eq!(model[0]["content"], "one");
    assert_eq!(model[1]["content"], "first");
    assert_eq!(model[2]["content"], "two");
}

#[test]
fn tool_trims_change_model_history_without_changing_the_transcript() {
    let user = json!({"role":"user", "content":"inspect"});
    let call = json!({"role":"assistant", "content":"", "tool_calls":[{"id":"call-1", "type":"function", "function":{"name":"read_file", "arguments":"{}"}}]});
    let original =
        json!({"role":"tool", "tool_call_id":"call-1", "content":"full original output"});
    let trimmed = json!({"role":"tool", "tool_call_id":"call-1", "content":"trimmed output"});
    let mut events: Vec<Event> = [user.clone(), call.clone(), original]
        .into_iter()
        .enumerate()
        .map(|(index, message)| Event {
            seq: index as i64 + 1,
            payload: EventPayload::Message {
                message,
                display: None,
            },
            created_at: 1,
        })
        .collect();
    let transcript = project_events(&events);
    events.push(Event {
        seq: 4,
        payload: EventPayload::ToolTrim {
            original_seq: 3,
            message: trimmed.clone(),
        },
        created_at: 2,
    });
    assert_eq!(
        surface_messages(&events),
        vec![user.clone(), call.clone(), trimmed]
    );
    assert_eq!(project_events(&events), transcript);
    let smaller = json!({"role":"tool", "tool_call_id":"call-1", "content":"smaller"});
    events.push(Event {
        seq: 5,
        payload: EventPayload::ToolTrim {
            original_seq: 3,
            message: smaller.clone(),
        },
        created_at: 3,
    });
    assert_eq!(surface_messages(&events), vec![user, call, smaller]);
    assert_eq!(project_events(&events), transcript);
}

#[test]
fn a_compaction_shadows_history_but_keeps_the_rendered_transcript() {
    let state = ChatState::in_memory();
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    state
        .store
        .begin_run(
            "s1",
            "r1",
            &store::NewUserTurn {
                message: json!({ "role": "user", "content": "old" }),
                display: Some(json!({ "text": "old" })),
                title: Some("old".into()),
            },
            2,
        )
        .unwrap();
    state
        .store
        .finish_run(&store::RunEnd {
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
            usage: Default::default(),
            error: None,
        })
        .unwrap();
    state
        .store
        .begin_run(
            "s1",
            "r2",
            &store::NewUserTurn {
                message: json!({ "role": "user", "content": "new" }),
                display: Some(json!({ "text": "new" })),
                title: None,
            },
            4,
        )
        .unwrap();
    let session = state
        .store
        .apply_compaction(
            "s1",
            &store::CompactionRecord {
                summary: json!({
                    "role": "user",
                    "content": "<compacted-summary>old turns</compacted-summary>",
                }),
                shadowed_through: 2,
                before: 100,
                after: 10,
                context: store::ContextUsage {
                    used: 10,
                    window: 100,
                },
            },
            5,
        )
        .unwrap();

    let model = surface_messages(&session.events);
    assert_eq!(model.len(), 2);
    assert_eq!(
        model[0]["content"],
        "<compacted-summary>old turns</compacted-summary>"
    );
    assert_eq!(model[1]["content"], "new");

    let items = project_events(&session.events);
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["kind"], "user");
    assert_eq!(items[1]["kind"], "assistant");
    assert_eq!(items[2]["kind"], "user");
    assert_eq!(items[3]["kind"], "notice");

    assert_eq!(state.store.history_count("s1").unwrap(), 2);
    let (history, total) = state.store.read_history("s1", 1, 10).unwrap();
    assert_eq!(total, 2);
    assert_eq!(history[0].1["content"], "old");
    assert_eq!(history[1].1["content"], "old answer");
    let _ = session;
}

#[test]
fn leading_checkpoint_text_stays_visible_in_strings_and_content_parts() {
    for content in [
        json!("<compacted-summary>quoted log</compacted-summary>"),
        json!([{"type":"text", "text":"<compacted-summary>quoted log</compacted-summary>"}]),
        json!("The earlier part of this conversation was dropped; explain this message"),
    ] {
        let message = json!({"role":"user", "content":content});
        assert!(!store::is_checkpoint(&message));
        let events = vec![Event {
            seq: 1,
            created_at: 0,
            payload: EventPayload::Message {
                message: message.clone(),
                display: None,
            },
        }];
        let items = project_events(&events);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], "user");
        assert_eq!(surface_messages(&events), [message]);
    }
}

#[test]
fn plan_cards_replace_their_duplicate_reply_and_keep_their_place() {
    let plan_event = |seq, version, status, steps: &str| Event {
        seq,
        created_at: 0,
        payload: EventPayload::Plan {
            version,
            status,
            steps: steps.into(),
            details: None,
            validation: None,
        },
    };
    let message = |seq, role: &str, text: &str| Event {
        seq,
        created_at: 0,
        payload: EventPayload::Message {
            message: json!({ "role": role, "content": text }),
            display: None,
        },
    };
    let events = vec![
        message(1, "assistant", "1. Add the worker."),
        plan_event(2, 1, store::PlanStatus::Pending, "1. Add the worker."),
        message(3, "user", "Implement plan v1"),
        plan_event(4, 1, store::PlanStatus::Accepted, "1. Add the worker."),
        message(5, "assistant", "1. Add the queue."),
        plan_event(6, 2, store::PlanStatus::Pending, "1. Add the queue."),
    ];

    let items = project_events(&events);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["kind"], "plan");
    assert_eq!(items[0]["version"], 1);
    assert_eq!(items[0]["status"], "accepted");
    assert_eq!(items[0]["latest"], false);
    assert_eq!(items[0]["seq"], 2);
    assert_eq!(items[1]["kind"], "user");
    assert_eq!(items[1]["text"], "Implement plan v1");
    assert_eq!(items[2]["kind"], "plan");
    assert_eq!(items[2]["version"], 2);
    assert_eq!(items[2]["status"], "pending");
    assert_eq!(items[2]["latest"], true);
    assert_eq!(items[2]["steps"], "1. Add the queue.");
}

#[test]
fn plan_cards_carry_the_backend_validation() {
    let validation = store::PlanValidation {
        status: store::PlanValidationStatus::Verified,
        valid: true,
        model_root: "architecture".into(),
        include_ignored: false,
        source_digest: "abc123".into(),
        diff_id: Some("worker".into()),
        dsl: "diff worker 'Worker' {}".into(),
        diff: Some(json!({
            "id": "worker",
            "title": "Worker",
            "added": { "elements": 1, "relations": 1 },
            "removed": { "elements": 0, "relations": 0 },
            "modified": { "elements": 0 },
            "views": [],
        })),
        diagnostics: Vec::new(),
        warnings: Vec::new(),
        overlaps: vec!["existing".into()],
        error: None,
    };
    let events = vec![Event {
        seq: 1,
        created_at: 0,
        payload: EventPayload::Plan {
            version: 1,
            status: store::PlanStatus::Pending,
            steps: "1. Add the worker.".into(),
            details: None,
            validation: Some(Box::new(validation)),
        },
    }];

    let items = project_events(&events);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "plan");
    assert_eq!(items[0]["validation"]["status"], "verified");
    assert_eq!(items[0]["validation"]["modelRoot"], "architecture");
    assert_eq!(items[0]["validation"]["includeIgnored"], false);
    assert_eq!(items[0]["validation"]["diffId"], "worker");
    assert_eq!(items[0]["validation"]["overlaps"][0], "existing");
    assert_eq!(items[0]["validation"]["dsl"], "diff worker 'Worker' {}");
}
