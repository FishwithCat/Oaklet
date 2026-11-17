use super::super::history::TOOL_IMAGE_NOTICE;
use super::*;
use crate::compaction::test_support::*;
use crate::compaction::trigger_at;

#[test]
fn forced_compaction_keeps_a_smaller_tail_than_normal_compaction() {
    assert!(force_retain_at(262_144) < retain_at(262_144));
    assert!(force_retain_at(8_192) < retain_at(8_192));
}

#[tokio::test]
async fn force_compacts_completed_tools_inside_the_current_turn() {
    let config = AppConfig {
        context_window: 8_192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let request = json!({"role":"user", "content":[
        {"type":"text", "text":"  Fix the build; never commit.  "},
        {"type":"image_url", "image_url":{"url":"data:image/png;base64,AA"}}
    ]});
    let mut messages = vec![system(), request.clone()];
    for index in 0..20 {
        messages.push(json!({"role":"assistant", "content":"", "tool_calls":[{
            "id":format!("call_{index}"), "type":"function", "function":{"name":"read_file", "arguments":"{}"}
        }]}));
        messages.push(tool(&format!("call_{index}"), &"x".repeat(2_000)));
    }
    assert!(estimate_tokens(&messages) > config.context_window);
    let next = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(estimate_tokens(&next.messages) <= trigger_at(config.context_window));
    assert_eq!(&next.messages[2..], &messages[messages.len() - 2..]);
    assert_eq!(
        &next.messages[1]["content"].as_array().unwrap()[1..],
        request["content"].as_array().unwrap().as_slice()
    );
    assert_eq!(next.messages[1]["content"][0]["type"], "text");
    assert_compaction_restores(&messages, &next);
    let mut longer = next.messages;
    longer.extend_from_slice(&messages[2..]);
    let again = force(
        &reqwest::Client::new(),
        &config,
        &longer,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(estimate_tokens(&again.messages) < estimate_tokens(&longer));
    assert_eq!(
        &again.messages[1]["content"].as_array().unwrap()[1..],
        request["content"].as_array().unwrap().as_slice()
    );
    assert!(
        !again.messages[1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<compacted-summary>\n<compacted-summary>")
    );
}

#[tokio::test]
async fn current_turn_compaction_uses_model_summary_when_the_prefix_fits() {
    let (base_url, seen, server) = summary_server();
    let config = AppConfig {
        context_window: 8_192,
        base_url,
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let request = "  Fix the build; never commit.  ";
    let mut messages = vec![system(), user(request)];
    for index in 0..10 {
        messages.push(json!({"role":"assistant", "content":"", "tool_calls":[{
            "id":format!("call_{index}"), "type":"function", "function":{"name":"read_file", "arguments":"{}"}
        }]}));
        messages.push(tool(&format!("call_{index}"), &"x".repeat(1_000)));
    }
    let next = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let checkpoint = next.messages[1]["content"].as_str().unwrap();
    assert!(checkpoint.contains("Goal: ship compaction."));
    assert!(checkpoint.ends_with(request));
    assert!(estimate_tokens(&next.messages) < estimate_tokens(&messages));
    assert_compaction_restores(&messages, &next);
}

#[tokio::test]
async fn forced_short_turn_drop_is_durable() {
    let config = AppConfig {
        context_window: 8_192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let current = "c".repeat(3_600);
    let messages = vec![
        system(),
        user(&"o".repeat(1_200)),
        assistant("done"),
        user(&current),
    ];
    let next = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        next.messages[1]["content"]
            .as_str()
            .unwrap()
            .contains(DROPPED)
    );
    assert!(
        next.messages[1]["content"]
            .as_str()
            .unwrap()
            .ends_with(&current)
    );
    assert_compaction_restores(&messages, &next);
}

#[tokio::test]
async fn dropped_middle_turn_restores_a_contiguous_tail_after_a_merged_checkpoint() {
    use crate::store::{
        CompactionEnd, CompactionRecord, ContextUsage, EventPayload, NewUserTurn, RunEnd, Store,
    };
    let store = Store::in_memory().unwrap();
    store.create_session("s", "/tmp", 1).unwrap();
    let turn = |message| NewUserTurn {
        message,
        display: None,
        title: None,
    };
    store.begin_run("s", "seed", &turn(user("old")), 2).unwrap();
    let answer_a = assistant(&"a".repeat(400));
    let request_b = user(&"b".repeat(800));
    let originals = vec![
        assistant("old done"),
        user("request A"),
        answer_a.clone(),
        request_b.clone(),
        assistant(&"b".repeat(800)),
    ];
    store
        .finish_run(&RunEnd {
            session_id: "s".into(),
            run_id: "seed".into(),
            start_seq: 1,
            status: "done",
            finished_at: 3,
            appended: originals
                .into_iter()
                .map(|message| EventPayload::Message {
                    message,
                    display: None,
                })
                .collect(),
            compaction: None,
            context: None,
            error: None,
        })
        .unwrap();
    let summary =
        user("<compacted-summary>Never commit without approval</compacted-summary>\n\nrequest A");
    store
        .apply_compaction(
            "s",
            &CompactionRecord {
                summary,
                shadowed_through: 3,
                before: 100,
                after: 10,
                context: ContextUsage {
                    used: 10,
                    window: 8192,
                },
            },
            4,
        )
        .unwrap();
    let current = user(&"c".repeat(3600));
    let start = store
        .begin_run("s", "r1", &turn(current.clone()), 5)
        .unwrap();
    let mut messages = vec![system()];
    messages.extend(start.prior_messages);
    messages.push(current);
    let config = AppConfig {
        context_window: 8192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let next = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!next.messages.contains(&request_b));
    let surface = next.messages[1..].to_vec();
    let checkpoint = surface[0]["content"].as_str().unwrap();
    assert!(checkpoint.contains("Never commit without approval"));
    assert!(checkpoint.contains("request A"));
    assert!(checkpoint.contains(answer_a["content"].as_str().unwrap()));
    let end = RunEnd {
        session_id: "s".into(),
        run_id: "r1".into(),
        start_seq: start.start_seq,
        status: "done",
        finished_at: 6,
        appended: vec![],
        compaction: Some(CompactionEnd {
            summary: Some(surface[0].clone()),
            surface: surface.clone(),
            before: 2000,
            after: 1000,
        }),
        context: None,
        error: None,
    };
    store.finish_run(&end).unwrap();
    store.finish_run(&end).unwrap();
    let restored = store
        .begin_run("s", "r2", &turn(user("continue")), 7)
        .unwrap();
    assert_eq!(restored.prior_messages, surface);
    let (archived, total) = store.read_history("s", 1, 20).unwrap();
    assert_eq!(total, 7);
    assert!(archived.iter().any(|(_, message)| message == &request_b));
    assert_eq!(store.history_count("s").unwrap(), total);
}

#[tokio::test]
async fn latest_tool_image_stays_with_its_call_even_above_the_retain_budget() {
    let config = AppConfig {
        context_window: 8192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let image = json!({"role":"user", "content":[{"type":"text","text":TOOL_IMAGE_NOTICE}, {"type":"image_url", "image_url":{"url":"data:image/png;base64,AA"}}]});
    let call = json!({"role":"assistant", "content":"", "tool_calls":[{"id":"image", "type":"function", "function":{"name":"read_image", "arguments":"{}"}}]});
    let result = tool("image", "Image loaded");
    let recent = vec![call, result, image];
    let mut single = vec![
        json!({"role":"system","content":"s".repeat(24000)}),
        user("Inspect screenshot"),
    ];
    single.extend(recent.clone());
    assert!(estimate_tokens(&single) > trigger_at(config.context_window));
    assert!(estimate_tokens(&single) < config.context_window);
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    assert!(
        force(
            &reqwest::Client::new(),
            &config,
            &single,
            &ToolRegistry::new(),
            &cancel
        )
        .await
        .unwrap()
        .is_none()
    );
    let mut longer = vec![system(), user("Inspect screenshot")];
    for index in 0..20 {
        longer.push(json!({"role":"assistant", "content":"", "tool_calls":[{"id":format!("old_{index}"),"type":"function","function":{"name":"read_file","arguments":"{}"}}]}));
        longer.push(tool(&format!("old_{index}"), &"x".repeat(2000)));
    }
    longer.extend(recent.clone());
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &longer,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        &compacted.messages[compacted.messages.len() - recent.len()..],
        recent.as_slice()
    );
    assert!(estimate_tokens(&compacted.messages) <= trigger_at(config.context_window));
    assert_compaction_restores(&longer, &compacted);
}

#[tokio::test]
async fn long_turn_fallback_does_not_repeat_the_live_request() {
    use crate::tools::CancelHandle;

    let request = format!("UNIQUE_REQUEST {}", "r".repeat(3_000));
    let mut messages = vec![system(), user(&request)];
    for index in 0..20 {
        messages.push(assistant(&format!("work {index}: {}", "w".repeat(200))));
        messages.push(tool(&format!("t{index}"), &"o".repeat(2_000)));
    }
    let config = AppConfig {
        context_window: 8_192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    let checkpoint = compacted.messages[1]["content"].as_str().unwrap();
    assert_eq!(checkpoint.matches("UNIQUE_REQUEST").count(), 1);
}

#[tokio::test]
async fn force_uses_a_digest_when_the_summary_prefix_does_not_fit() {
    use crate::tools::CancelHandle;

    let messages = tool_history(20);
    let config = AppConfig {
        base_url: "invalid://no-summary".into(),
        context_window: 8_192,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    let checkpoint = compacted.messages[1]["content"].as_str().unwrap();

    assert!(checkpoint.contains("Fix the build; preserve public APIs."));
    assert!(checkpoint.contains("read_file("));
    assert!(!checkpoint.contains(DROPPED));
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    assert!(
        compacted
            .replaced
            .iter()
            .any(|message| message == &messages[3])
    );
}

#[test]
fn drops_the_oldest_turn_but_keeps_the_checkpoint_and_current_request() {
    let messages = vec![
        system(),
        user("<compacted-summary>\nold work\n</compacted-summary>"),
        user("old request"),
        assistant("old answer"),
        user("current request"),
        assistant("current answer"),
    ];

    let (next, removed) = drop_oldest_turn(&messages).unwrap();
    let removed = &messages[removed];

    assert_eq!(next.len(), 3);
    assert_eq!(removed, &messages[1..4]);
    assert!(
        next[1]["content"]
            .as_str()
            .unwrap()
            .contains("<compacted-summary>")
    );
    assert!(
        !next
            .iter()
            .any(|message| message["content"] == "old request")
    );
    assert!(
        next[1]["content"]
            .as_str()
            .unwrap()
            .ends_with("current request")
    );
    assert!(next[1]["content"].as_str().unwrap().contains("old work"));
}

#[test]
fn keeps_the_dropped_notice_while_shedding_old_turns() {
    let messages = vec![
        system(),
        user(DROPPED),
        user("old request"),
        assistant("old answer"),
        user("current request"),
    ];

    let (next, removed) = drop_oldest_turn(&messages).unwrap();

    assert_eq!(next.len(), 2);
    assert_eq!(removed, 1..4);
    assert!(next[1]["content"].as_str().unwrap().contains(DROPPED));
    assert!(
        !next
            .iter()
            .any(|message| message["content"] == "old request")
    );
    assert!(
        next.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .ends_with("current request")
    );
}

#[tokio::test]
async fn force_summarizes_with_a_smaller_tail() {
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
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .expect("forced compaction should apply");
    let next = compacted.messages;

    server.join().unwrap();
    assert!(!compacted.replaced.is_empty());
    assert!(compacted.replaced.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("request 0 "))
    }));
    assert!(
        compacted
            .replaced
            .iter()
            .all(|message| message["role"] != "system")
    );
    assert!(estimate_tokens(&next) < estimate_tokens(&messages));
    assert_eq!(next.last().unwrap(), &last);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let body: Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(body["tool_choice"], "none");
    assert!(body.get("tools").is_some());
    // Sampling parameters are optional; some providers reject them.
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn force_archives_the_untrimmed_tool_output_before_summarizing() {
    use crate::tools::CancelHandle;

    let (base_url, _seen, server) = summary_server();
    let original = format!(
        "{}\nUNIQUE ERROR: disk full\n{}",
        "h".repeat(4_000),
        "t".repeat(4_000)
    );
    let mut messages = vec![
        system(),
        user("old request"),
        assistant("reading"),
        tool("call_1", &original),
    ];
    for step in 0..30 {
        messages.push(user(&format!("request {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user("current request"));

    let config = AppConfig {
        base_url,
        context_window: 32_768,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .expect("forced compaction should apply");

    server.join().unwrap();
    assert!(
        compacted
            .replaced
            .iter()
            .any(|message| { message["content"].as_str() == Some(original.as_str()) })
    );
}

#[tokio::test]
async fn force_skips_the_summary_when_trimming_tool_output_is_enough() {
    use crate::tools::CancelHandle;

    let original = format!(
        "HEAD\n{}\nUNIQUE ERROR: disk full\n{}",
        "x".repeat(4_000),
        "y".repeat(4_000)
    );
    let messages = vec![
        system(),
        user("old request"),
        assistant("reading"),
        tool("call_1", &original),
        user("current request"),
    ];
    let config = AppConfig {
        base_url: "http://127.0.0.1:1".into(),
        context_window: 32_768,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();

    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .expect("trimmable tool output should count as progress");
    let next = compacted.messages;

    assert_eq!(compacted.replaced.len(), 1);
    assert_eq!(
        compacted.replaced[0]["content"], original,
        "the untrimmed tool result is archived for history_read"
    );
    assert!(
        compacted.replaced[0]["content"]
            .as_str()
            .unwrap()
            .contains("UNIQUE ERROR: disk full")
    );
    assert!(
        next.iter()
            .any(|message| message["content"] == "old request")
    );
    assert_eq!(next.last().unwrap()["content"], "current request");
    assert!(next.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("tool output trimmed for context recovery"))
    }));
}

#[tokio::test]
async fn force_keeps_a_bounded_digest_when_the_summary_prefix_would_overflow() {
    use crate::tools::CancelHandle;

    let mut messages = vec![system()];
    for step in 0..40 {
        messages.push(user(&format!("request {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    let last = messages.last().unwrap().clone();

    // The prefix is far beyond this window, so force must not spend a
    // summary request that is guaranteed to be rejected.
    let config = AppConfig {
        base_url: "http://127.0.0.1:1".into(),
        context_window: 8_192,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let compacted = force(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .expect("forced compaction should apply");
    let next = compacted.messages;

    assert!(!compacted.replaced.is_empty());
    assert!(estimate_tokens(&next) < estimate_tokens(&messages));
    assert_eq!(next.last().unwrap(), &last);
    assert!(next.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("user: request 0"))
    }));
    assert!(estimate_tokens(&next) <= trigger_at(config.context_window));
}

#[tokio::test]
async fn long_turn_summary_budget_includes_tool_schemas() {
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = AppConfig {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        context_window: 8_192,
        ..AppConfig::default()
    };
    let mut messages = tool_history(10);
    messages.pop();
    // Short results leave the prefix itself within budget; schemas push it over.
    for message in &mut messages {
        if role(message) == Some("tool") {
            message["content"] = json!("x".repeat(1_000));
        }
    }
    let tools = large_tool_registry();
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let compacted = force(&client, &config, &messages, &tools, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    assert_compaction_restores(&messages, &compacted);
}

#[tokio::test]
async fn forced_compaction_does_not_grow_history_when_the_system_prompt_dominates() {
    let config = AppConfig {
        context_window: 4_096,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let messages = vec![
        json!({"role": "system", "content": "s".repeat(8_000)}),
        user("hi"),
        assistant("hello"),
        user("continue"),
    ];
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    assert!(
        force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel
        )
        .await
        .unwrap()
        .is_none()
    );
}
