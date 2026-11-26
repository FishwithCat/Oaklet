use super::transcript::view_from_session;
use super::*;
use crate::store::EventPayload;

#[test]
fn stopping_a_confirmation_prevents_the_handoff_to_implementation() {
    let chat = ChatState::in_memory();
    let (handle, cancel) = CancelHandle::new();
    let confirmation = chat
        .reserve_plan_confirmation("s1", "main", handle.clone())
        .unwrap();
    assert!(chat.window_busy("main"));
    assert!(
        chat.reserve_plan_confirmation("s1", "main", handle)
            .is_err()
    );
    assert!(
        chat.live()
            .check_plan_confirmation("s1", "main", &cancel)
            .is_ok()
    );
    chat.live().stop_session_operations("s1", "other");
    assert!(!cancel.is_cancelled());
    chat.live().stop_session_operations("s1", "main");
    assert_eq!(
        chat.live()
            .check_plan_confirmation("s1", "main", &cancel)
            .unwrap_err(),
        "Stopped."
    );
    drop(confirmation);
    assert!(!chat.window_busy("main"));
}

#[tokio::test]
async fn draining_waits_for_cancelled_compaction_to_settle() {
    let state = ChatState::in_memory();
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let (handle, cancel) = CancelHandle::new();
    state.live().compacting.insert(
        "s1".into(),
        super::SessionOperation {
            window: "main".into(),
            cancel: handle,
        },
    );
    state.live().compacting.get("s1").unwrap().cancel();
    assert!(cancel.is_cancelled());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            state.drain(Some("main"))
        )
        .await
        .is_err()
    );
    state.live().compacting.remove("s1");
    state.drain(Some("main")).await.unwrap();
}

#[test]
fn injected_store_recovers_stale_runs_and_shares_subsequent_writes() {
    let store = store::Store::in_memory().unwrap();
    store.create_session("shared", "/workspace", 1).unwrap();
    store
        .begin_run(
            "shared",
            "stale",
            &store::NewUserTurn {
                message: json!({"role":"user","content":"hello"}),
                display: None,
                title: None,
            },
            2,
        )
        .unwrap();
    let chat = ChatState::from_store(store.clone()).unwrap();
    let session = store.get_session("shared").unwrap().unwrap();
    assert!(session.events.iter().any(
        |e| matches!(&e.payload,EventPayload::Notice {text,..} if text == super::INTERRUPTED_NOTICE)
    ));
    store.create_session("later", "/workspace", 3).unwrap();
    assert!(chat.store.get_session("later").unwrap().is_some());
    assert_eq!(store.interrupt_stale_runs(4, &json!({})).unwrap(), 0);
}

#[tokio::test]
async fn a_failed_close_retains_the_reply_until_a_retry_can_save_it() {
    let state = ChatState::in_memory();
    let end = Arc::new(store::RunEnd {
        session_id: "s1".into(),
        run_id: "r1".into(),
        start_seq: 1,
        status: "stopped",
        finished_at: 3,
        appended: vec![EventPayload::Message {
            message: json!({ "role": "assistant", "content": "partial answer" }),
            display: None,
        }],
        compaction: None,
        context: None,
        usage: Default::default(),
        error: None,
    });
    state
        .live()
        .unsaved
        .insert("r1".into(), ("main".into(), end));
    assert!(state.drain(Some("main")).await.is_err());
    assert!(state.live().unsaved.contains_key("r1"));
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    state
        .store
        .begin_run(
            "s1",
            "r1",
            &store::NewUserTurn {
                message: json!({ "role": "user", "content": "hello" }),
                display: None,
                title: None,
            },
            2,
        )
        .unwrap();
    state.drain(Some("main")).await.unwrap();
    assert!(state.live().unsaved.is_empty());
    assert_eq!(
        state.store.get_session("s1").unwrap().unwrap().events.len(),
        2
    );
}

#[test]
fn live_runs_override_the_persisted_view() {
    let state = ChatState::in_memory();
    let session = state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let view = SessionView {
        id: "s1".into(),
        workspace: "/tmp/ws".into(),
        title: String::new(),
        updated_at: 1,
        turns: 0,
        items: vec![json!({
            "kind": "tool",
            "runId": "r1",
            "id": "t1",
            "name": "run_command",
            "status": "awaiting",
            "summary": "ls",
        })],
        first_seq: None,
        has_more: false,
        context: None,
        last_usage: None,
        pending: Vec::new(),
        running: true,
        blocked: true,
        compacting: false,
        active_run_id: Some("r1".into()),
    };
    let (cancel, _cancel) = CancelHandle::new();
    state.live().runs.insert(
        "r1".into(),
        RunHandle {
            session_id: "s1".into(),
            window: "main".into(),
            cancel,
            view: Arc::new(Mutex::new(view)),
        },
    );

    let live = state.live_view("s1").unwrap();
    assert!(live.running);
    assert_eq!(live.active_run_id.as_deref(), Some("r1"));

    let summaries = state.summaries(vec![store::SessionSummary {
        id: "s1".into(),
        title: String::new(),
        updated_at: 1,
        turns: 0,
    }]);
    assert!(summaries[0].running);
    assert!(summaries[0].blocked);

    let idle = view_from_session(session, false);
    assert!(!idle.running);
}

fn busy_state() -> ChatState {
    let state = ChatState::in_memory();
    let session = state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let mut view = view_from_session(session, false);
    view.running = true;
    view.active_run_id = Some("r1".into());
    let (cancel, _cancel) = CancelHandle::new();
    state.live().runs.insert(
        "r1".into(),
        RunHandle {
            session_id: "s1".into(),
            window: "main".into(),
            cancel,
            view: Arc::new(Mutex::new(view)),
        },
    );
    state
}

fn queued_turn(text: &str) -> store::NewUserTurn {
    store::NewUserTurn {
        message: json!({ "role": "user", "content": text }),
        display: Some(json!({ "text": text })),
        title: None,
    }
}

#[test]
fn a_steer_joins_the_queue_and_is_claimed_once() {
    let state = busy_state();
    let result = state
        .enqueue_input(
            "s1",
            "main",
            SendMode::Steer,
            "p1".into(),
            queued_turn("go left"),
            |pending| {
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].text, "go left");
            },
        )
        .unwrap();
    assert!(matches!(
        result,
        Ok(ref session) if session.active_run_id.as_deref() == Some("r1")
    ));
    assert!(state.has_pending_steer("s1"));

    let mut published = 0;
    let (taken, pending) = state.take_steer_inputs("s1", |items, pending| {
        published = items.len();
        assert_eq!(items[0]["text"], "go left");
        assert!(pending.is_empty());
    });
    assert_eq!(published, 1);
    assert_eq!(taken.len(), 1);
    assert!(taken[0].created_at > 0);
    assert!(pending.is_empty());
    assert!(!state.has_pending_steer("s1"));

    let view = state.live_view("s1").unwrap();
    assert!(view.pending.is_empty());
    assert!(
        view.items
            .iter()
            .any(|item| item["kind"] == "user" && item["text"] == "go left")
    );
    assert!(state.take_steer_inputs("s1", |_, _| {}).0.is_empty());
}

#[test]
fn a_queued_message_waits_for_the_next_turn() {
    let state = busy_state();
    let enqueued = state
        .enqueue_input(
            "s1",
            "main",
            SendMode::Queue,
            "p1".into(),
            queued_turn("later"),
            |_| {},
        )
        .unwrap();
    assert!(enqueued.is_ok());

    assert!(!state.has_pending_steer("s1"));
    let (taken, pending) = state.take_steer_inputs("s1", |_, _| {});
    assert!(taken.is_empty());
    assert_eq!(pending.len(), 1);

    assert_eq!(pending[0].text, "later");
}

#[test]
fn a_normal_send_is_not_queued_behind_a_run() {
    let state = busy_state();
    let result = state
        .enqueue_input(
            "s1",
            "main",
            SendMode::Normal,
            "p1".into(),
            queued_turn("hello"),
            |_| panic!("a normal send must not publish a pending row"),
        )
        .unwrap();
    assert!(matches!(
        result,
        Err(turn) if turn.display.as_ref().and_then(|display| display["text"].as_str())
            == Some("hello")
    ));
}

#[test]
fn the_queue_is_bounded() {
    let state = busy_state();
    for index in 0..MAX_PENDING {
        let result = state
            .enqueue_input(
                "s1",
                "main",
                SendMode::Steer,
                format!("p{index}"),
                queued_turn("go"),
                |_| {},
            )
            .unwrap();
        assert!(result.is_ok());
    }

    let error = match state.enqueue_input(
        "s1",
        "main",
        SendMode::Steer,
        "overflow".into(),
        queued_turn("go"),
        |_| {},
    ) {
        Err(error) => error,
        Ok(_) => panic!("the queue must reject a message over its bound"),
    };
    assert!(error.contains("16"));
    assert_eq!(state.pending_views("s1").len(), MAX_PENDING);
}

fn compacting_state() -> ChatState {
    let state = ChatState::in_memory();
    state.store.create_session("s1", "/tmp/ws", 1).unwrap();
    let (cancel, _cancel) = CancelHandle::new();
    state.live().compacting.insert(
        "s1".into(),
        SessionOperation {
            window: "main".into(),
            cancel,
        },
    );
    state
}

#[test]
fn a_normal_send_waits_as_a_steer_while_compacting() {
    let state = compacting_state();
    let mut published: Vec<String> = Vec::new();
    let result = state
        .enqueue_input(
            "s1",
            "main",
            SendMode::Normal,
            "p1".into(),
            queued_turn("after compaction"),
            |pending| published = pending.iter().map(|row| row.text.clone()).collect(),
        )
        .unwrap();
    let Ok(session) = result else {
        panic!("a compaction holds the message instead of rejecting it");
    };
    assert_eq!(published, ["after compaction"]);
    assert!(session.compacting);
    assert!(!session.running);
    assert_eq!(session.pending.len(), 1);

    let pending = state.pending_views("s1");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].mode, InputMode::Steer);
    assert!(state.live_view("s1").is_none());
}

#[test]
fn another_window_cannot_queue_on_this_compaction() {
    let state = compacting_state();
    let error = match state.enqueue_input(
        "s1",
        "other",
        SendMode::Steer,
        "p1".into(),
        queued_turn("go"),
        |_| {},
    ) {
        Err(error) => error,
        Ok(_) => panic!("another window must not queue on this compaction"),
    };
    assert!(error.contains("another window"));
    assert!(state.pending_views("s1").is_empty());
}
