use super::test_support::*;
use super::*;
use crate::context::{MessageId, identify, is_request};
use crate::tools::CancelHandle;

fn history() -> Vec<Value> {
    vec![
        system(),
        identify(user(&"old work ".repeat(2000)), MessageId::Stored(1)),
        identify(assistant(&"completed ".repeat(1000)), MessageId::Stored(2)),
        identify(
            user("Oaklet loop guard: explain this; never commit."),
            MessageId::Stored(3),
        ),
        identify(assistant("Latest answer"), MessageId::Pending(0)),
    ]
}

fn config(base_url: String) -> AppConfig {
    AppConfig {
        base_url,
        context_window: 32768,
        ..AppConfig::default()
    }
}

#[tokio::test]
async fn all_triggers_keep_the_request_separate_and_verbatim() {
    for trigger in [Trigger::Automatic, Trigger::Manual, Trigger::Overflow] {
        let (url, seen, server) = summary_server();
        let original = history();
        let (_handle, cancel) = CancelHandle::new();
        let next = compact(
            &reqwest::Client::new(),
            &config(url),
            &original,
            &ToolRegistry::new(),
            trigger,
            &cancel,
        )
        .await
        .unwrap()
        .unwrap();
        server.join().unwrap();
        assert!(crate::store::is_checkpoint(&next[1]));
        assert!(next.iter().any(|message| message == &original[3]));
        assert_eq!(next.last(), original.last());
        assert!(is_request(
            next.iter()
                .find(|message| *message == &original[3])
                .unwrap()
        ));
        let body: Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
        assert!(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| message.get("oaklet_id").is_none())
        );
    }
}

#[tokio::test]
async fn errors_never_return_a_mechanical_replacement() {
    for (status, response) in [
        (500, "unavailable"),
        (
            200,
            r#"{"choices":[{"message":{"content":""},"finish_reason":"stop"}]}"#,
        ),
        (
            200,
            r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"length"}]}"#,
        ),
        (200, "malformed"),
        (
            200,
            r#"{"choices":[{"message":{"content":"unconfirmed completion"}}]}"#,
        ),
    ] {
        let (url, _, server) = summary_server_with(vec![(status, response)]);
        let messages = history();
        let original = messages.clone();
        let (_handle, cancel) = CancelHandle::new();
        assert!(
            compact(
                &reqwest::Client::new(),
                &config(url),
                &messages,
                &ToolRegistry::new(),
                Trigger::Manual,
                &cancel
            )
            .await
            .is_err()
        );
        server.join().unwrap();
        assert_eq!(messages, original);
    }
}

#[tokio::test]
async fn cancellation_preserves_the_input() {
    let (handle, cancel) = CancelHandle::new();
    handle.cancel();
    assert!(
        compact(
            &reqwest::Client::new(),
            &config("invalid://summary".into()),
            &history(),
            &ToolRegistry::new(),
            Trigger::Manual,
            &cancel
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn manual_compaction_skips_short_history_without_a_summary_request() {
    let (_handle, cancel) = CancelHandle::new();
    for (system_text, current_request) in [
        ("system".to_string(), "continue".to_string()),
        ("s".repeat(40_000), "r".repeat(20_000)),
    ] {
        let messages = vec![
            json!({"role":"system", "content":system_text}),
            user("hi"),
            assistant("hello"),
            user(&current_request),
            assistant("latest answer"),
        ];
        let next = compact(
            &reqwest::Client::new(),
            &config("invalid://summary-must-not-be-called".into()),
            &messages,
            &ToolRegistry::new(),
            Trigger::Manual,
            &cancel,
        )
        .await
        .unwrap();
        assert!(next.is_none());
    }
}

#[tokio::test]
async fn manual_compaction_rechecks_the_shorter_prefix_before_requesting_a_summary() {
    let (_handle, cancel) = CancelHandle::new();
    let messages = vec![
        json!({"role":"system", "content":"s".repeat(20_000)}),
        user(&"u".repeat(4_000)),
        assistant(&"a".repeat(12_000)),
        user("continue"),
        assistant("latest answer"),
    ];
    let config = AppConfig {
        summary_model: "small".into(),
        summary_context_window: Some(8192),
        ..config("invalid://summary-must-not-be-called".into())
    };
    let next = compact(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap();
    assert!(next.is_none());
}

#[tokio::test]
async fn an_unhelpful_summary_is_rejected_after_framing() {
    let response = json!({"choices":[{
        "finish_reason":"stop",
        "message":{"content":"s".repeat(16_384)}
    }]})
    .to_string();
    let (url, _, server) = summary_server_with(vec![(200, &response)]);
    let (_handle, cancel) = CancelHandle::new();
    let messages = vec![
        system(),
        user(&"u".repeat(15_984)),
        assistant("done"),
        user("continue"),
        assistant("yes"),
    ];
    let error = compact(
        &reqwest::Client::new(),
        &config(url),
        &messages,
        &ToolRegistry::new(),
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap_err();
    server.join().unwrap();
    assert!(error.contains("did not shrink"));
}

#[test]
fn synthetic_sources_and_batches_do_not_hide_a_real_request() {
    let mut messages = vec![
        system(),
        user("Images returned by the previous tool call(s). Explain this text."),
    ];
    for index in 0..8 {
        messages.push(json!({"role":"assistant", "tool_calls":[
            {"id":format!("a{index}")}, {"id":format!("b{index}")}
        ]}));
        messages.push(tool(&format!("a{index}"), &"x".repeat(2000)));
        messages.push(tool(&format!("b{index}"), "done"));
        messages.push(context::source(user("tool image"), "tool_images"));
        messages.push(context::source(user("reminder"), "reminder"));
    }
    let plan = history::plan(&messages, 1000).unwrap().unwrap();
    assert_eq!(plan.request, Some(1));
    assert_eq!(messages[plan.cut]["role"], "assistant");
    assert_eq!(plan.cut, messages.len() - 5);
    messages.pop();
    messages.pop();
    messages.pop();
    assert!(history::plan(&messages, 1000).is_err());
}

#[tokio::test]
async fn a_smaller_summary_window_selects_one_shorter_balanced_prefix() {
    let (url, seen, server) = summary_server();
    let config = AppConfig {
        summary_model: "small".into(),
        summary_context_window: Some(8192),
        ..config(url)
    };
    let mut messages = vec![system(), user("Goal: finish")];
    for index in 0..12 {
        messages.push(assistant(&format!("step {index} {}", "x".repeat(4000))));
    }
    let (_handle, cancel) = CancelHandle::new();
    let next = compact(
        &reqwest::Client::new(),
        &config,
        &messages,
        &ToolRegistry::new(),
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0]).unwrap();
    assert_eq!(body["model"], "small");
    assert!(
        context::estimate_tokens(body["messages"].as_array().unwrap())
            + body["max_tokens"].as_u64().unwrap() as usize
            <= 8192
    );
    assert!(
        next.len() > 4,
        "the unprocessed suffix remains available for the next attempt"
    );
}

#[tokio::test]
async fn summary_replays_images_instead_of_silently_omitting_them() {
    let (url, seen, server) = summary_server();
    let mut messages = history();
    let image = json!({"type":"image_url", "image_url":{"url":format!("data:image/png;base64,{}", "A".repeat(20000))}});
    messages[1]["content"] =
        json!([{"type":"text", "text":"old requirements ".repeat(2000)}, image.clone()]);
    let (_handle, cancel) = CancelHandle::new();
    compact(
        &reqwest::Client::new(),
        &config(url),
        &messages,
        &ToolRegistry::new(),
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap();
    server.join().unwrap();
    let body: Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert_eq!(body["messages"][1]["content"][1], image);
}

#[test]
fn a_separate_summary_model_requires_its_capacity() {
    let config = AppConfig {
        summary_model: "other".into(),
        ..config("invalid://summary".into())
    };
    assert!(summary::prefix_fits(&history(), 3, &config, None).is_err());
}

#[test]
fn trimming_is_bounded_and_keeps_the_spill_pointer() {
    let path = "/tmp/oaklet/spill/session-0123456789abcdef/read_file-call_1.txt";
    let text = format!(
        "{}\n\n[... tool output middle pruned; full output (900 lines, 20000 bytes) saved to: {path}. Use read_file with offset and limit to page through it.]\n\n{}",
        "h".repeat(4000),
        "t".repeat(4000)
    );
    let original = vec![tool("c", &text)];
    let trimmed = trim_tool_outputs(&original).unwrap();
    assert!(trimmed[0]["content"].as_str().unwrap().chars().count() <= 4500);
    assert_eq!(
        crate::tools::spill_pointer(trimmed[0]["content"].as_str().unwrap()),
        Some(path)
    );
    assert!(trim_tool_outputs(&trimmed).is_none());
}

#[tokio::test]
async fn cancellation_after_summary_headers_does_not_wait_for_the_body() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        stream.read_exact(&mut vec![0; length]).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10000\r\n\r\n")
            .unwrap();
        let _ = headers_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(3));
    });
    let (handle, cancel) = CancelHandle::new();
    let running = tokio::spawn(async move {
        summary::summarize(
            &reqwest::Client::new(),
            &config(url),
            &[system(), user("old")],
            None,
            &cancel,
        )
        .await
    });
    headers_rx.await.unwrap();
    handle.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn summary_budget_omits_schemas_when_only_the_replay_fits() {
    let (url, seen, server) = summary_server();
    let (_handle, cancel) = CancelHandle::new();
    let config = AppConfig {
        context_window: 8192,
        ..config(url)
    };
    let tools = large_tool_registry();
    compact(
        &reqwest::Client::new(),
        &config,
        &history(),
        &tools,
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    let body: Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert!(body.get("tools").is_none());
    assert!(
        context::estimate_tokens(body["messages"].as_array().unwrap())
            + body["max_tokens"].as_u64().unwrap() as usize
            <= config.context_window
    );
}

#[tokio::test]
async fn normal_finish_reasons_are_accepted_and_oversized_output_is_rejected() {
    for reason in ["stop", "end_turn", "stop_sequence"] {
        let response =
            json!({"choices":[{"finish_reason":reason,"message":{"content":"Goal: finish."}}]})
                .to_string();
        let (url, _, server) = summary_server_with(vec![(200, &response)]);
        let (_handle, cancel) = CancelHandle::new();
        assert!(
            compact(
                &reqwest::Client::new(),
                &config(url),
                &history(),
                &ToolRegistry::new(),
                Trigger::Manual,
                &cancel
            )
            .await
            .unwrap()
            .is_some()
        );
        server.join().unwrap();
    }
    let response =
        json!({"choices":[{"finish_reason":"stop","message":{"content":"x".repeat(20000)}}]})
            .to_string();
    let (url, _, server) = summary_server_with(vec![(200, &response)]);
    let (_handle, cancel) = CancelHandle::new();
    assert!(
        compact(
            &reqwest::Client::new(),
            &config(url),
            &history(),
            &ToolRegistry::new(),
            Trigger::Manual,
            &cancel
        )
        .await
        .unwrap_err()
        .contains("token budget")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn a_schema_parameter_rejection_retries_the_same_replay_without_tools() {
    let (url, seen, server) = summary_server_with(vec![
        (400, "tool_choice is unsupported"),
        (200, SUMMARY_BODY),
    ]);
    let (_handle, cancel) = CancelHandle::new();
    compact(
        &reqwest::Client::new(),
        &config(url),
        &history(),
        &large_tool_registry(),
        Trigger::Manual,
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_str(&requests[0]).unwrap();
    let second: Value = serde_json::from_str(&requests[1]).unwrap();
    assert!(first.get("tools").is_some());
    assert!(second.get("tools").is_none());
    assert_eq!(first["messages"], second["messages"]);
}
