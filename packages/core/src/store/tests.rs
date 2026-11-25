//! Store behavior: run lifecycle, paging, usage, and durability.

use serde_json::json;

use super::{
    ContextUsage, EventPayload, NewUserTurn, PlanState, PlanStatus, PlanValidation,
    PlanValidationStatus, RunEnd, RunUsage, StepUsage, Store, has_table, is_checkpoint,
};
use crate::agent::Usage;
use crate::context::{ContextItem, ContextKind, ContextSegment};

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
fn recognizes_checkpoints_in_string_and_array_content() {
    assert!(is_checkpoint(&super::checkpoint(json!({
        "role": "user",
        "content": "<compacted-summary>old</compacted-summary>",
    }))));
    assert!(is_checkpoint(&super::checkpoint(json!({
        "role": "user",
        "content": [
            { "type": "text", "text": "<compacted-summary>old</compacted-summary>" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } },
        ],
    }))));
    assert!(!is_checkpoint(&json!({
        "role": "user",
        "content": [{ "type": "text", "text": "ordinary" }],
    })));
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
    assert!(first.window.last_usage.is_none());
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
                segments: vec![ContextSegment {
                    kind: ContextKind::ToolResults,
                    tokens: 12,
                    items: vec![ContextItem {
                        label: "read_file · src/main.rs".into(),
                        tokens: 12,
                    }],
                }],
            }),
            usage: RunUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                cache_hit_tokens: 60,
                cache_miss_tokens: 40,
                steps: 2,
                last_prompt_tokens: 60,
                last_completion_tokens: 15,
                last_cache_hit_tokens: 40,
                last_cache_miss_tokens: 20,
            },
            error: None,
        })
        .unwrap();

    assert_eq!(saved.events.len(), 2);
    assert_eq!(saved.events[1].seq, 2);
    assert_eq!(
        saved.context,
        Some(ContextUsage {
            used: 12,
            window: 100,
            segments: vec![ContextSegment {
                kind: ContextKind::ToolResults,
                tokens: 12,
                items: vec![ContextItem {
                    label: "read_file · src/main.rs".into(),
                    tokens: 12,
                }],
            }],
        })
    );
    assert_eq!(
        saved.last_usage,
        Some(StepUsage {
            prompt_tokens: 60,
            completion_tokens: 15,
            cache_hit_tokens: 40,
            cache_miss_tokens: 20,
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
    let usage: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT prompt_tokens, completion_tokens, cache_hit_tokens, \
             cache_miss_tokens, model_steps, last_prompt_tokens, \
             last_completion_tokens, last_cache_hit_tokens, last_cache_miss_tokens \
             FROM runs WHERE id = 'r1'",
            [],
            |row| {
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
                ))
            },
        )
        .unwrap();
    assert_eq!(usage, (100, 20, 60, 40, 2, 60, 15, 40, 20));
}

#[test]
fn run_usage_accumulates_steps_and_keeps_provider_counts_verbatim() {
    let mut total = RunUsage::default();
    total.add(Some(Usage {
        prompt_tokens: 100,
        completion_tokens: 5,
        cache_hit_tokens: 60,
        cache_miss_tokens: 40,
    }));
    total.add(None);
    total.add(Some(Usage {
        prompt_tokens: 200,
        completion_tokens: 10,
        cache_hit_tokens: 50,
        cache_miss_tokens: 100,
    }));

    assert_eq!(
        total,
        RunUsage {
            prompt_tokens: 300,
            completion_tokens: 15,
            cache_hit_tokens: 110,
            cache_miss_tokens: 140,
            steps: 3,
            last_prompt_tokens: 200,
            last_completion_tokens: 10,
            last_cache_hit_tokens: 50,
            last_cache_miss_tokens: 100,
        }
    );
}

#[test]
fn new_runs_start_with_zero_usage() {
    let store = store();
    store.create_session("s1", "/tmp/a", 1).unwrap();
    store
        .begin_run("s1", "r1", &turn("hello", None), 2)
        .unwrap();
    let conn = store.connection().unwrap();
    let usage: (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT prompt_tokens, completion_tokens, cache_hit_tokens, \
             cache_miss_tokens, model_steps FROM runs WHERE id = 'r1'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(usage, (0, 0, 0, 0, 0));
}

#[test]
fn plan_state_survives_runs_and_advances_versions() {
    let store = store();
    store.create_session("s1", "/tmp/a", 1).unwrap();
    let started = store.begin_run("s1", "r1", &turn("plan", None), 2).unwrap();
    assert_eq!(started.plan, PlanState::default());
    store
        .finish_run(&RunEnd {
            session_id: "s1".into(),
            run_id: "r1".into(),
            start_seq: started.start_seq,
            status: "done",
            finished_at: 3,
            appended: vec![EventPayload::Plan {
                version: 1,
                status: PlanStatus::Pending,
                steps: String::new(),
                details: None,
                validation: None,
            }],
            compaction: None,
            context: None,
            usage: Default::default(),
            error: None,
        })
        .unwrap();
    assert_eq!(
        store.plan_state("s1").unwrap(),
        PlanState {
            latest: Some(1),
            pending: true,
            requires_validation: false,
        }
    );
    let started = store
        .begin_run("s1", "r2", &turn("revise", None), 4)
        .unwrap();
    assert_eq!(started.plan.latest, Some(1));
    assert!(started.plan.pending);
}

#[test]
fn plan_confirmation_consumes_the_latest_version_once() {
    let store = store();
    store.create_session("s1", "/tmp/a", 1).unwrap();
    let started = store.begin_run("s1", "r1", &turn("plan", None), 2).unwrap();
    let validation = PlanValidation {
        status: PlanValidationStatus::Verified,
        valid: true,
        model_root: "architecture".into(),
        include_ignored: false,
        source_digest: "abc123".into(),
        diff_id: Some("worker".into()),
        dsl: "diff worker 'Add worker' { add { extend api { worker = component 'Worker' } } }"
            .into(),
        diff: Some(json!({ "id": "worker", "title": "Add worker" })),
        diagnostics: Vec::new(),
        warnings: Vec::new(),
        overlaps: vec!["existing".into()],
        error: None,
    };
    let details = crate::store::PlanDetails {
        objective: "Add a worker".into(),
        changes: "Introduce a queue.".into(),
        steps: vec![crate::store::PlanStep {
            title: "Add the queue".into(),
            outcome: "Jobs can be enqueued.".into(),
            files: Vec::new(),
            components: vec!["api.queue".into()],
            depends_on: Vec::new(),
            verification: "cargo test queue".into(),
        }],
        risks: Vec::new(),
        questions: Vec::new(),
    };
    store
        .finish_run(&RunEnd {
            session_id: "s1".into(),
            run_id: "r1".into(),
            start_seq: started.start_seq,
            status: "done",
            finished_at: 3,
            appended: vec![EventPayload::Plan {
                version: 1,
                status: PlanStatus::Pending,
                steps: "1. Add the worker.".into(),
                details: Some(Box::new(details.clone())),
                validation: Some(Box::new(validation.clone())),
            }],
            compaction: None,
            context: None,
            usage: Default::default(),
            error: None,
        })
        .unwrap();

    let plan = store.latest_plan("s1").unwrap().unwrap();
    assert_eq!(plan.version, 1);
    assert_eq!(plan.steps, "1. Add the worker.");
    assert_eq!(plan.details, Some(details.clone()));
    assert_eq!(plan.validation, Some(validation.clone()));

    assert!(
        store
            .begin_plan_run("s1", "stale", &turn("old", None), 0, 4)
            .is_err()
    );
    let started = store
        .begin_plan_run("s1", "r2", &turn("implement", None), 1, 4)
        .unwrap();
    assert!(started.plan.pending);
    assert_eq!(
        store.plan_state("s1").unwrap(),
        PlanState {
            latest: Some(1),
            pending: false,
            requires_validation: false,
        }
    );
    let plan = store.latest_plan("s1").unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Accepted);
    assert_eq!(plan.steps, "1. Add the worker.");
    assert_eq!(plan.details, Some(details));
    assert_eq!(plan.validation, Some(validation));
    assert!(
        store
            .begin_plan_run("s1", "r3", &turn("again", None), 1, 5)
            .is_err()
    );
}

#[test]
fn retrying_a_finished_write_does_not_duplicate_the_reply() {
    let store = store();
    store.create_session("s1", "/tmp/a", 1).unwrap();
    store
        .begin_run("s1", "r1", &turn("hello", None), 2)
        .unwrap();
    let end = RunEnd {
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
    };
    store.finish_run(&end).unwrap();
    let retried = store.finish_run(&end).unwrap();
    assert_eq!(retried.events.len(), 2);
    assert_eq!(retried.events[1].seq, 2);
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
fn a_message_that_quotes_a_checkpoint_marker_is_not_a_checkpoint() {
    assert!(!is_checkpoint(&json!({
        "role": "user",
        "content": "The log printed <compacted-summary> and then stopped.",
    })));
    assert!(!is_checkpoint(&json!({
        "role": "user",
        "content": "note: The earlier part of this conversation was dropped",
    })));
    assert!(!is_checkpoint(&json!({
        "role": "user",
        "content": "<compacted-summary>old work</compacted-summary>",
    })));
    assert!(!is_checkpoint(&json!({
        "role": "user",
        "content": "The earlier part of this conversation was dropped so the run could stay \
                    within the context window.",
    })));
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
                usage: Default::default(),
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
            usage: Default::default(),
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
fn refuses_a_pre_v0_database_without_resetting() {
    let root = crate::tools::test_support::temp_root("store-pre-v0");
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

    let error = Store::open(&path).err().expect("must fail");
    assert!(error.contains("predates"), "{error}");

    let conn = rusqlite::Connection::open(&path).unwrap();
    let old: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = 'old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old, 1);
    assert!(has_table(&conn, "session_messages").unwrap());
    assert!(!has_table(&conn, "schema_migrations").unwrap());
    drop(conn);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reports_session_storage_per_workspace() {
    let store = store();
    store.create_session("a1", "/tmp/a", 1).unwrap();
    store.create_session("a2", "/tmp/a", 2).unwrap();
    store.create_session("b1", "/tmp/b", 3).unwrap();
    store
        .begin_run("a1", "r1", &turn("one", Some("one")), 4)
        .unwrap();

    let usage = store.usage().unwrap();
    assert_eq!(usage.sessions, 3);
    assert_eq!(usage.messages, 1);
    assert_eq!(usage.database_bytes, 0);

    let first = &usage.workspaces[0];
    assert_eq!(first.workspace, "/tmp/a");
    assert_eq!(first.sessions, 2);
    assert_eq!(first.messages, 1);
    assert!(first.bytes > 0);
    assert_eq!(first.updated_at, 4);

    let second = &usage.workspaces[1];
    assert_eq!(second.workspace, "/tmp/b");
    assert_eq!(second.sessions, 1);
    assert_eq!(second.messages, 0);
    assert_eq!(second.bytes, 0);
}

#[test]
fn purge_removes_one_workspace_or_every_session() {
    let store = store();
    store.create_session("a1", "/tmp/a", 1).unwrap();
    store.create_session("a2", "/tmp/a", 2).unwrap();
    store.create_session("b1", "/tmp/b", 3).unwrap();
    store
        .begin_run("a1", "r1", &turn("one", Some("one")), 4)
        .unwrap();
    store
        .begin_run("b1", "r2", &turn("two", Some("two")), 5)
        .unwrap();

    assert!(store.has_workspace("/tmp/a").unwrap());
    assert!(!store.has_workspace("/tmp/missing").unwrap());

    let mut removed = store.purge(Some("/tmp/a")).unwrap();
    removed.sort();
    assert_eq!(removed, ["a1", "a2"]);
    assert!(store.get_session("a1").unwrap().is_none());
    assert!(!store.has_workspace("/tmp/a").unwrap());
    assert_eq!(
        store
            .list_sessions("/tmp/b")
            .unwrap()
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["b1"]
    );
    assert_eq!(store.usage().unwrap().sessions, 1);

    assert_eq!(store.purge(None).unwrap(), ["b1"]);
    let usage = store.usage().unwrap();
    assert_eq!(usage.sessions, 0);
    assert_eq!(usage.messages, 0);
    assert!(usage.workspaces.is_empty());
}

#[cfg(unix)]
#[test]
fn keeps_store_files_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let root = crate::tools::test_support::temp_root("store-permissions");
    let path = root.join("oaklet.db");
    let store = Store::open(&path).unwrap();
    drop(store);

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&path), 0o600);
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sibling = std::path::PathBuf::from(name);
        if sibling.exists() {
            assert_eq!(mode(&sibling), 0o600);
        }
    }
    std::fs::remove_dir_all(root).ok();
}
