use super::checkpoint::DROPPED;
use super::history::{FORCE_TOOL_OUTPUT_CHARS, role};
use super::test_support::*;
use super::*;
use std::net::TcpListener;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

mod manual;

#[test]
fn leaves_room_for_the_response_before_compacting() {
    assert_eq!(trigger_at(262_144), 209_715);
    assert_eq!(trigger_at(32_768), 24_576);
    assert_eq!(trigger_at(4_096), 3_072);
}

#[test]
fn keeps_recent_history_proportional_to_the_window() {
    assert_eq!(retain_at(262_144), 43_690);
    assert_eq!(retain_at(4_096), 2_000);
}

#[tokio::test]
async fn pressure_trimming_fits_twenty_results_without_a_summary() {
    use crate::tools::CancelHandle;

    let messages = tool_history(20);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = AppConfig {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        context_window: 32_768,
        ..AppConfig::default()
    };
    assert!(estimate_tokens(&messages) > trigger_at(config.context_window));
    let (_handle, cancel) = CancelHandle::new();
    let client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let compacted = compact(
        &client,
        &config,
        &messages,
        estimate_tokens(&messages),
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(estimate_tokens(&compacted.messages) <= trigger_at(config.context_window));
    assert_eq!(compacted.messages.len(), messages.len());
    assert_eq!(
        compacted.replaced,
        messages
            .iter()
            .filter(|message| role(message) == Some("tool"))
            .cloned()
            .collect::<Vec<_>>()
    );
    for (original, next) in messages.iter().zip(&compacted.messages) {
        if role(original) == Some("tool") {
            assert_eq!(original["tool_call_id"], next["tool_call_id"]);
            let text = next["content"].as_str().unwrap();
            assert!(text.chars().count() <= PRESSURE_TOOL_OUTPUT_CHARS);
            assert!(text.chars().count() > FORCE_TOOL_OUTPUT_CHARS);
        } else {
            assert_eq!(original, next);
        }
    }
}

#[tokio::test]
async fn pressure_trimming_observes_cancellation() {
    use crate::tools::CancelHandle;

    let messages = tool_history(20);
    let (handle, cancel) = CancelHandle::new();
    handle.cancel();
    let result = compact(
        &reqwest::Client::new(),
        &AppConfig::default(),
        &messages,
        estimate_tokens(&messages),
        &ToolRegistry::new(),
        &cancel,
    )
    .await;
    assert!(matches!(result, Err(error) if error == "Stopped."));
}

#[tokio::test]
async fn insufficient_pressure_trimming_summarizes_the_original_prefix() {
    use crate::tools::CancelHandle;

    // Provider-reported overhead requires a summary even though trimming fits locally.
    for (count, overhead) in [(40, 100_000), (20, 100_000)] {
        let (base_url, seen, server) = summary_server();
        let messages = tool_history(count);
        let config = AppConfig {
            base_url,
            context_window: 131_072,
            ..AppConfig::default()
        };
        let cut = select_cut(&messages, retain_at(config.context_window), MIN_REPLACED).unwrap();
        let tools = ToolRegistry::new();
        let (_handle, cancel) = CancelHandle::new();
        let compacted = compact(
            &reqwest::Client::new(),
            &config,
            &messages,
            estimate_tokens(&messages) + overhead,
            &tools,
            &cancel,
        )
        .await
        .unwrap()
        .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let body: Value = serde_json::from_str(&seen[0]).unwrap();
        let replayed = body["messages"].as_array().unwrap();
        assert_eq!(&replayed[..cut], &messages[..cut]);
        assert_eq!(replayed.len(), cut + 1);
        assert_eq!(body["tools"], tools.schemas());
        assert_eq!(compacted.replaced, messages[1..cut]);
        drop(seen);
        server.join().unwrap();
    }
}

#[tokio::test]
async fn automatic_summary_failures_keep_a_recallable_mechanical_checkpoint() {
    use crate::store::{ContextUsage, EventPayload, RunEnd, Store};
    use crate::tools::history::HistoryRead;
    use crate::tools::test_support::context;
    use crate::tools::{CancelHandle, Tool};

    let oversized = json!({"choices": [{"message": {"content": "z".repeat(100_000)}}]}).to_string();
    for (status, payload) in [
        (200, r#"{"choices":[{"message":{"content":""}}]}"#),
        (200, oversized.as_str()),
        (500, r#"{"error":{"message":"unavailable"}}"#),
    ] {
        let (base_url, _seen, server) = summary_server_with(vec![(status, payload)]);
        let config = AppConfig {
            base_url,
            context_window: 32_768,
            ..AppConfig::default()
        };
        let mut messages = tool_history(3);
        messages[7]["content"] = json!("error: disk full");
        let cut = messages.len() - 1;
        let (_handle, cancel) = CancelHandle::new();
        let compacted = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            cut,
            Some(&ToolRegistry::new()),
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        server.join().unwrap();

        let checkpoint = compacted.messages[1]["content"].as_str().unwrap();
        assert!(checkpoint.contains("Fix the build; preserve public APIs."));
        assert!(checkpoint.contains("The failure is located; the fix is still pending."));
        assert!(checkpoint.contains("read_file({\"path\":\"build.log\"})"));
        assert!(checkpoint.contains("error: disk full"));
        assert!(!checkpoint.contains("UNIQUE ERROR"));
        assert!(!checkpoint.contains(DROPPED));
        assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
        assert_eq!(compacted.replaced, messages[1..cut]);

        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        store
            .begin_run(
                "s1",
                "seed",
                &crate::store::NewUserTurn {
                    message: messages[1].clone(),
                    display: None,
                    title: None,
                },
                1,
            )
            .unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "seed".into(),
                start_seq: 1,
                status: "done",
                finished_at: 1,
                appended: messages[2..]
                    .iter()
                    .map(|message| EventPayload::Message {
                        message: message.clone(),
                        display: None,
                    })
                    .collect(),
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        store
            .apply_compaction(
                "s1",
                &crate::store::CompactionRecord {
                    summary: compacted.messages[1].clone(),
                    shadowed_through: cut as i64 - 1,
                    before: estimate_tokens(&messages),
                    after: estimate_tokens(&compacted.messages),
                    context: ContextUsage {
                        used: estimate_tokens(&compacted.messages),
                        window: config.context_window,
                    },
                },
                2,
            )
            .unwrap();
        let history = HistoryRead::new(store, "s1".into(), Arc::new(StdMutex::new(Vec::new())));
        let (ctx, root, _handle) = context("digest-recall");
        let original = history
            .execute(&ctx, json!({"offset": 3, "limit": 1}))
            .await
            .unwrap();
        assert!(original.contains("UNIQUE ERROR: disk full"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn summary_that_grows_after_framing_falls_back_to_a_digest() {
    use crate::tools::CancelHandle;

    let summary = "x".repeat(7_900);
    let body =
        format!(r#"{{"choices":[{{"message":{{"role":"assistant","content":"{summary}"}}}}]}}"#);
    let (base_url, _seen, server) = summary_server_with(vec![(200, body.as_str())]);
    let config = AppConfig {
        context_window: 8_192,
        base_url,
        ..AppConfig::default()
    };
    let messages = vec![
        system(),
        user(&"a".repeat(4_000)),
        assistant(&"b".repeat(4_000)),
        user("continue"),
    ];
    let (_handle, cancel) = CancelHandle::new();
    let compacted = compact_at(
        &reqwest::Client::new(),
        &config,
        &messages,
        3,
        None,
        &cancel,
        CompactMode::Auto,
    )
    .await
    .unwrap();
    server.join().unwrap();
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    assert!(
        !compacted.messages[1]["content"]
            .as_str()
            .unwrap()
            .contains(&summary)
    );
}

#[tokio::test]
async fn replaces_the_middle_with_the_model_summary() {
    use crate::tools::CancelHandle;

    let (base_url, seen, server) = summary_server();

    let mut messages = vec![system()];
    for step in 0..30 {
        messages.push(user(&format!("request {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    let last = messages.last().unwrap().clone();

    let config = AppConfig {
        base_url,
        context_window: 32_768,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let client = reqwest::Client::new();
    let tools = ToolRegistry::new();

    let compacted = compact(
        &client,
        &config,
        &messages,
        estimate_tokens(&messages),
        &tools,
        &cancel,
    )
    .await
    .unwrap()
    .expect("compaction should apply");
    let next = compacted.messages;

    server.join().unwrap();
    assert!(compacted.replaced.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("request 0"))
    }));
    assert!(
        compacted
            .replaced
            .iter()
            .all(|message| message["role"] != "system")
    );
    assert!(next.len() < messages.len());
    assert_eq!(next[0]["role"], "system");
    assert!(next.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("ship compaction."))
    }));
    assert_eq!(next.last().unwrap(), &last);

    let seen = seen.lock().unwrap();
    let body: Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(body["model"], config.model);
    assert_eq!(body["tool_choice"], "none");
    assert!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "read_file")
    );
    let replayed = body["messages"].as_array().unwrap();
    assert_eq!(replayed[0]["content"], "You are Oaklet.");
    assert_eq!(replayed.last().unwrap()["role"], "user");
    assert!(
        replayed.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("at most 800 words")
    );
    assert!(replayed.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("request 0"))
    }));
}

#[tokio::test]
async fn retries_without_tools_when_a_provider_rejects_tool_choice() {
    use crate::tools::CancelHandle;

    let (base_url, seen, server) = summary_server_with(vec![
        (
            400,
            r#"{"error":{"message":"unsupported parameter: tool_choice"}}"#,
        ),
        (200, SUMMARY_BODY),
    ]);
    let mut messages = vec![system()];
    for step in 0..30 {
        messages.push(user(&format!("request {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    let config = AppConfig {
        base_url,
        context_window: 32_768,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let tools = ToolRegistry::new();

    compact(
        &reqwest::Client::new(),
        &config,
        &messages,
        estimate_tokens(&messages),
        &tools,
        &cancel,
    )
    .await
    .unwrap()
    .expect("compaction should apply");

    server.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let first: Value = serde_json::from_str(&seen[0]).unwrap();
    let second: Value = serde_json::from_str(&seen[1]).unwrap();
    assert_eq!(first["tool_choice"], "none");
    assert!(first.get("tools").is_some());
    assert!(second.get("tools").is_none());
    assert!(second.get("tool_choice").is_none());
    assert_eq!(second["messages"], first["messages"]);
}

#[tokio::test]
async fn every_entry_observes_cancellation_before_rewriting_history() {
    let config = AppConfig {
        context_window: 1_024,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (handle, cancel) = crate::tools::CancelHandle::new();
    handle.cancel();
    let client = reqwest::Client::new();
    let tools = ToolRegistry::new();
    for messages in [Vec::new(), tool_history(20)] {
        assert!(matches!(
            force(&client, &config, &messages, &tools, &cancel).await,
            Err(error) if error == "Stopped."
        ));
        assert!(matches!(
            compact_messages(&client, &config, &messages, &cancel).await,
            Err(error) if error == "Stopped."
        ));
    }
}

#[tokio::test]
async fn automatic_compaction_skips_an_oversized_summary_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = AppConfig {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        context_window: 8_192,
        ..AppConfig::default()
    };
    let messages = tool_history(20);
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let compacted = compact(
        &client,
        &config,
        &messages,
        estimate_tokens(&messages),
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    assert!(
        compacted.messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Fix the build")
    );
}
