use super::*;
use crate::AppConfig;
use crate::agent::Usage;
use crate::chat::{RunHandle, SendMode, test_support::state};
use crate::compaction::test_support::user;
use crate::store::NewUserTurn;
use crate::tools::CancelHandle;
use crate::tools::{ArchitectureImpact, Assessment, Intent, ProcessOutcome};

#[test]
fn repairs_pending_events_for_unanswered_tool_calls() {
    let mut pending = vec![
        EventPayload::Message {
            message: json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    { "id": "a", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
                    { "id": "b", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
                ],
            }),
            display: Some(json!({ "runId": "r1" })),
        },
        EventPayload::Message {
            message: json!({ "role": "tool", "tool_call_id": "a", "content": "" }),
            display: Some(json!({
                "runId": "r1",
                "name": "read_file",
                "pending": true,
            })),
        },
    ];

    close_incomplete_tool_calls(&mut pending, "r1");
    assert_eq!(pending.len(), 3);
    let EventPayload::Message { message, display } = &pending[1] else {
        panic!("expected a tool message");
    };
    assert!(
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("unknown"))
    );
    assert_eq!(
        display
            .as_ref()
            .and_then(|display| display.get("pending"))
            .and_then(serde_json::Value::as_bool),
        None
    );
    let EventPayload::Message { message, .. } = &pending[2] else {
        panic!("expected a synthetic tool result");
    };
    assert_eq!(message["tool_call_id"], "b");
}

#[tokio::test]
async fn reasoning_time_is_saved_for_completed_and_interrupted_replies() {
    for status in ["done", "stopped", "error"] {
        let (state, store, root) = state(AppConfig::default());
        store.create_session("s", "/tmp", 1).unwrap();
        let request = user("hi");
        let started = store
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
        let ctx = RunContext::without_window(state, "s", "r", started);
        ctx.reasoning("thinking");
        ctx.reasoning(" more");
        let mut messages = vec![request];
        if status == "done" {
            ctx.delta("answer");
            messages.push(ctx.record_message(
                json!({
                    "role": "assistant", "content": "answer", "reasoning_content": "thinking more",
                }),
                None,
            ));
            ctx.finish_done(messages).await;
        } else if status == "stopped" {
            ctx.finish_stopped(messages).await;
        } else {
            ctx.finish_error(messages, "failed").await;
        }
        let saved = store.get_session("s").unwrap().unwrap();
        let display = saved
            .events
            .iter()
            .find_map(|event| match &event.payload {
                crate::store::EventPayload::Message { message, display }
                    if message["reasoning_content"] == "thinking more" =>
                {
                    display.as_ref()
                }
                _ => None,
            })
            .unwrap();
        assert!(
            display["reasoningDurationMs"].as_u64().is_some(),
            "{status}"
        );
        let (model, _) = store.context_snapshot("s").unwrap();
        assert!(model.last().unwrap().get("reasoningDurationMs").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn a_panicking_agent_settles_the_run_as_an_error() {
    let (state, store, root) = state(AppConfig::default());
    store.create_session("s", "/tmp", 1).unwrap();
    let request = user("hi");
    let started = store
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
    let ctx = RunContext::without_window(state, "s", "r", started);

    crate::chat::settle_on_panic(async { panic!("boom") }, ctx, vec![request]).await;

    let saved = store.get_session("s").unwrap().unwrap();
    assert!(saved.events.iter().any(|event| matches!(
        &event.payload,
        crate::store::EventPayload::Notice { tone, text }
            if tone == "error" && text.contains("boom")
    )));
    // The run is terminal, so there is no stale running row to interrupt.
    assert_eq!(store.interrupt_stale_runs(10, &json!({})).unwrap(), 0);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_finished_run_saves_the_usage_accumulated_across_steps() {
    let (state, store, root) = state(AppConfig::default());
    store.create_session("s", "/tmp", 1).unwrap();
    let request = user("hi");
    let started = store
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
    let ctx = RunContext::without_window(state, "s", "r", started);
    ctx.record_usage(Some(Usage {
        prompt_tokens: 100,
        completion_tokens: 5,
        cache_hit_tokens: 60,
        cache_miss_tokens: 40,
    }));
    ctx.record_usage(Some(Usage {
        prompt_tokens: 200,
        completion_tokens: 10,
        cache_hit_tokens: 50,
        cache_miss_tokens: 150,
    }));
    assert_eq!(
        ctx.usage(),
        store::RunUsage {
            prompt_tokens: 300,
            completion_tokens: 15,
            cache_hit_tokens: 110,
            cache_miss_tokens: 190,
            steps: 2,
            last_prompt_tokens: 200,
            last_completion_tokens: 10,
            last_cache_hit_tokens: 50,
            last_cache_miss_tokens: 150,
        }
    );
    ctx.finish_done(vec![request]).await;

    assert!(store.get_session("s").unwrap().is_some());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_command_outcome_is_saved_with_the_tool_message() {
    let (state, store, root) = state(AppConfig::default());
    store.create_session("s", "/tmp", 1).unwrap();
    let request = user("run the tests");
    let started = store
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
    let ctx = RunContext::without_window(state, "s", "r", started);
    ctx.tool_start("c1", "run_command", "cargo test", &[]);
    ctx.tool_result(
        "c1",
        false,
        "Exit code 101.\nfailed",
        &[],
        Some(&ProcessOutcome::Exited { code: 101 }),
    );
    ctx.finish_done(vec![request]).await;

    let saved = store.get_session("s").unwrap().unwrap();
    let display = saved
        .events
        .iter()
        .find_map(|event| match &event.payload {
            crate::store::EventPayload::Message { message, display }
                if message["tool_call_id"] == "c1" =>
            {
                display.as_ref()
            }
            _ => None,
        })
        .expect("the tool result should be saved");
    assert_eq!(display["ok"], false);
    assert_eq!(display["outcome"]["kind"], "exited");
    assert_eq!(display["outcome"]["code"], 101);
    std::fs::remove_dir_all(root).ok();
}

fn live_run() -> (RunContext, store::Store, std::path::PathBuf) {
    let (state, store, root) = state(AppConfig::default());
    store.create_session("s", "/tmp", 1).unwrap();
    let started = store
        .begin_run(
            "s",
            "r",
            &NewUserTurn {
                message: user("hi"),
                display: None,
                title: None,
            },
            2,
        )
        .unwrap();
    let ctx = RunContext::without_window(state, "s", "r", started);
    let (cancel, _cancel) = CancelHandle::new();
    ctx.state.chat.live().runs.insert(
        "r".into(),
        RunHandle {
            session_id: "s".into(),
            window: "test".into(),
            cancel,
            view: ctx.view.clone(),
        },
    );
    (ctx, store, root)
}

#[tokio::test]
async fn a_planning_turn_records_a_pending_plan() {
    let (ctx, store, root) = live_run();
    ctx.access().assess(Assessment {
        intent: Intent::Evaluate,
        impact: ArchitectureImpact::Confirmed,
    });
    let validation = store::PlanValidation {
        status: store::PlanValidationStatus::Verified,
        valid: true,
        model_root: "architecture".into(),
        include_ignored: false,
        source_digest: "abc".into(),
        diff_id: Some("worker".into()),
        dsl: "diff worker 'Worker' {}".into(),
        diff: None,
        diagnostics: Vec::new(),
        warnings: Vec::new(),
        overlaps: Vec::new(),
        error: None,
    };
    *ctx.plan_validation
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(validation.clone());
    let details = store::PlanDetails {
        objective: "Add a worker".into(),
        changes: "Introduce a queue.".into(),
        steps: vec![store::PlanStep {
            title: "Add the queue".into(),
            outcome: "Jobs can be enqueued.".into(),
            files: vec!["src/queue.rs".into()],
            components: vec!["api.queue".into()],
            depends_on: Vec::new(),
            verification: "cargo test queue".into(),
        }],
        risks: vec!["Delivery is at least once.".into()],
        questions: Vec::new(),
    };
    *ctx.plan_details
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(details.clone());
    ctx.record_message(
        json!({"role":"assistant", "content":"1. Add the worker."}),
        None,
    );
    ctx.finish_done(Vec::new()).await;

    let session = store.get_session("s").unwrap().unwrap();
    let plan = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::Plan {
                version,
                status,
                steps,
                details,
                validation,
            } => Some((
                *version,
                *status,
                steps.clone(),
                details.clone(),
                validation.clone(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(plan.0, 1);
    assert_eq!(plan.1, store::PlanStatus::Pending);
    assert_eq!(plan.2, "1. Add the worker.");
    assert_eq!(plan.3.as_deref(), Some(&details));
    assert_eq!(plan.4.as_deref(), Some(&validation));
    assert_eq!(
        store.plan_state("s").unwrap(),
        store::PlanState {
            latest: Some(1),
            pending: true,
            requires_validation: true,
        }
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn architecture_plans_and_revisions_cannot_skip_validation() {
    let (ctx, store, root) = live_run();
    ctx.access().assess(Assessment {
        intent: Intent::Implement,
        impact: ArchitectureImpact::Confirmed,
    });
    ctx.record_message(json!({"role":"assistant", "content":"Add a worker."}), None);
    ctx.finish_done(Vec::new()).await;

    let turn = NewUserTurn {
        message: user("revise"),
        display: None,
        title: None,
    };
    let plan = store.latest_plan("s").unwrap().unwrap();
    assert_eq!(
        plan.validation.unwrap().status,
        store::PlanValidationStatus::Required
    );
    assert!(store.begin_plan_run("s", "skip", &turn, 1, 3).is_err());

    let started = store.begin_run("s", "revision", &turn, 4).unwrap();
    let revision = RunContext::without_window(ctx.state.clone(), "s", "revision", started);
    revision.access().assess(Assessment {
        intent: Intent::Revise,
        impact: ArchitectureImpact::None,
    });
    revision.record_message(
        json!({"role":"assistant", "content":"Update the worker steps."}),
        None,
    );
    revision.finish_done(Vec::new()).await;
    let plan = store.latest_plan("s").unwrap().unwrap();
    assert_eq!(plan.version, 2);
    assert_eq!(
        plan.validation.unwrap().status,
        store::PlanValidationStatus::Required
    );
    assert!(store.begin_plan_run("s", "skip", &turn, 2, 5).is_err());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_direct_implementation_turn_records_no_plan() {
    let (ctx, store, root) = live_run();
    ctx.access().assess(Assessment {
        intent: Intent::Implement,
        impact: ArchitectureImpact::None,
    });
    ctx.record_message(json!({"role":"assistant", "content":"done"}), None);
    ctx.finish_done(Vec::new()).await;

    let session = store.get_session("s").unwrap().unwrap();
    assert!(
        !session
            .events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::Plan { .. }))
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn finishing_twice_saves_the_reply_and_settles_the_run_only_once() {
    let (ctx, store, root) = live_run();
    ctx.record_message(json!({"role":"assistant", "content":"answer"}), None);
    ctx.finish_done(Vec::new()).await;
    let saved = store.get_session("s").unwrap().unwrap();
    assert_eq!(saved.events.len(), 2);
    assert!(ctx.state.chat.live().runs.is_empty());
    assert!(!ctx.view.lock().unwrap().running);

    ctx.finish_error(Vec::new(), "late failure").await;
    assert_eq!(
        store.get_session("s").unwrap().unwrap().events,
        saved.events
    );
    assert!(ctx.state.chat.live().unsaved.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_failed_finish_keeps_the_partial_reply_until_its_window_drains() {
    let (ctx, store, root) = live_run();
    let blocker = rusqlite::Connection::open(root.join("oaklet.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    ctx.delta("partial answer");
    ctx.finish_stopped(Vec::new()).await;
    assert!(ctx.state.chat.live().runs.is_empty());
    assert!(ctx.state.chat.live().unsaved.contains_key("r"));
    let view = ctx.view.lock().unwrap().clone();
    assert!(!view.running);
    assert!(
        view.items
            .iter()
            .any(|item| item["text"] == "partial answer")
    );
    assert_eq!(store.get_session("s").unwrap().unwrap().events.len(), 1);
    blocker.execute_batch("ROLLBACK").unwrap();

    ctx.state.chat.drain(Some("another-window")).await.unwrap();
    assert!(ctx.state.chat.live().unsaved.contains_key("r"));
    ctx.state.chat.drain(Some("test")).await.unwrap();
    assert!(ctx.state.chat.live().unsaved.is_empty());
    let saved = store.get_session("s").unwrap().unwrap();
    assert_eq!(
        saved
            .events
            .iter()
            .filter(|event| matches!(
                &event.payload, EventPayload::Message { message, .. }
                    if message["content"] == "partial answer"
            ))
            .count(),
        1
    );
    ctx.state.chat.drain(None).await.unwrap();
    assert_eq!(
        store.get_session("s").unwrap().unwrap().events,
        saved.events
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn queue_message(ctx: &RunContext, text: &str) {
    let turn = NewUserTurn {
        message: json!({ "role": "user", "content": text }),
        display: Some(json!({ "text": text })),
        title: None,
    };
    let queued = ctx
        .state
        .chat
        .enqueue_input(
            &ctx.session_id,
            "test",
            SendMode::Steer,
            format!("pending-{text}"),
            turn,
            |_| {},
        )
        .unwrap();
    assert!(queued.is_ok());
}

#[tokio::test]
async fn queued_input_survives_write_failures_until_its_own_turn_commits() {
    let (ctx, store, root) = live_run();
    queue_message(&ctx, "first");
    queue_message(&ctx, "second");

    let blocker = rusqlite::Connection::open(root.join("oaklet.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    ctx.finish_done(Vec::new()).await;
    assert!(ctx.state.chat.live().unsaved.contains_key("r"));
    assert!(ctx.state.chat.live().runs.is_empty());
    assert_eq!(ctx.state.chat.pending_views("s").len(), 2);
    assert_eq!(store.get_session("s").unwrap().unwrap().turns, 1);
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(ctx.prepare_resume("/tmp").is_err());
    ctx.state.chat.drain(Some("test")).await.unwrap();

    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(ctx.prepare_resume("/tmp").is_err());
    assert_eq!(ctx.state.chat.pending_views("s").len(), 2);
    assert_eq!(store.get_session("s").unwrap().unwrap().turns, 1);
    blocker.execute_batch("ROLLBACK").unwrap();

    let next = ctx.prepare_resume("/tmp").unwrap().unwrap();
    let view = next.ctx.view.lock().unwrap();
    assert_eq!(view.pending.len(), 1);
    assert_eq!(view.pending[0].text, "second");
    assert_eq!(view.items.last().unwrap()["text"], "first");
    assert_eq!(store.get_session("s").unwrap().unwrap().turns, 2);
    drop(view);
    assert!(ctx.prepare_resume("/tmp").is_err());
    assert_eq!(ctx.state.chat.pending_views("s")[0].text, "second");
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_failed_automatic_turn_does_not_start_another() {
    let (mut ctx, store, root) = live_run();
    ctx.auto_resume = true;
    queue_message(&ctx, "leftover");
    ctx.finish_error(Vec::new(), "provider error").await;
    assert!(
        ctx.state.chat.pending_views("s").is_empty(),
        "an automatic retry must not loop after another error"
    );
    assert_eq!(store.get_session("s").unwrap().unwrap().turns, 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_steer_is_claimed_once_at_a_step_boundary() {
    let (ctx, _store, root) = live_run();
    queue_message(&ctx, "go left");
    assert!(ctx.has_pending_steer());

    let taken = ctx.take_steer_inputs();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].display["text"], "go left");
    assert!(!ctx.has_pending_steer());
    assert!(
        ctx.view
            .lock()
            .unwrap()
            .items
            .iter()
            .any(|item| item["kind"] == "user" && item["text"] == "go left")
    );
    assert!(ctx.take_steer_inputs().is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn stopping_drops_the_queue_without_adding_a_turn() {
    let (ctx, store, root) = live_run();
    queue_message(&ctx, "late");
    ctx.finish_stopped(Vec::new()).await;

    let saved = store.get_session("s").unwrap().unwrap();
    assert_eq!(saved.turns, 1);
    assert!(ctx.state.chat.pending_views("s").is_empty());
    assert!(!saved.events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::Message { message, .. } if message["content"] == "late"
    )));
    std::fs::remove_dir_all(root).ok();
}
