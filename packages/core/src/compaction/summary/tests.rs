use super::*;
use crate::compaction::test_support::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[test]
fn summary_prefix_budget_counts_the_tool_schemas_it_replays() {
    let messages = vec![
        system(),
        user(&"u".repeat(8_000)),
        assistant(&"a".repeat(8_000)),
        user("current"),
    ];
    let config = AppConfig {
        context_window: 8_192,
        ..AppConfig::default()
    };
    let cut = messages.len() - 1;
    assert!(prefix_fits(&messages, cut, &config, None));

    let tools = large_tool_registry();
    assert!(!prefix_fits(&messages, cut, &config, Some(&tools)));
}

#[test]
fn summary_messages_replay_tool_history_before_the_directive() {
    let messages = vec![
        json!({ "role": "system", "content": "" }),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" }
            }]
        }),
        tool("call_1", "file body"),
    ];
    let replayed = summary_messages(&messages);
    assert_eq!(replayed.len(), 3);
    assert_eq!(
        replayed[0]["tool_calls"][0]["function"]["name"],
        "read_file"
    );
    assert_eq!(replayed[1]["content"], "file body");
    assert_eq!(replayed[2]["role"], "user");
    assert!(
        replayed[2]["content"]
            .as_str()
            .unwrap()
            .contains("checkpoint")
    );
}

#[test]
fn summary_uses_the_main_model_unless_overridden() {
    let mut config = AppConfig::default();
    assert_eq!(summary_model(&config), config.model.as_str());
    config.summary_model = "cheap-model".into();
    assert_eq!(summary_model(&config), "cheap-model");
}

#[tokio::test]
async fn cancelling_a_summary_after_headers_does_not_wait_for_its_body() {
    use crate::tools::CancelHandle;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        let mut byte = [0];
        while !raw.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            raw.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
            .unwrap();
        headers_sent.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(5));
    });
    let config = AppConfig {
        base_url: format!("http://{address}"),
        ..AppConfig::default()
    };
    let (handle, cancel) = CancelHandle::new();
    let client = reqwest::Client::new();
    let body = json!({});
    let request = post_summary(&client, &config, &body, &cancel);
    tokio::pin!(request);
    tokio::select! {
        _ = headers_received => {},
        _ = &mut request => panic!("summary finished before its body arrived"),
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(20)) => {},
        _ = &mut request => panic!("summary finished before cancellation"),
    }
    handle.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), request).await;
    let _ = release.send(());
    server.join().unwrap();
    assert_eq!(result.unwrap().unwrap_err(), "Stopped.");
}

#[tokio::test]
async fn rejects_truncated_and_oversized_summary_responses() {
    let oversized = format!(
        r#"{{"choices":[{{"finish_reason":"stop","message":{{"content":"{}"}}}}]}}"#,
        "字".repeat(4097)
    );
    for response in [
        r#"{"choices":[{"finish_reason":"length","message":{"content":"PARTIAL SUMMARY"}}]}"#,
        oversized.as_str(),
    ] {
        let (base_url, _, server) = summary_server_with(vec![(200, response)]);
        let config = AppConfig {
            base_url,
            ..AppConfig::default()
        };
        let (_handle, cancel) = crate::tools::CancelHandle::new();
        let result = summarize(
            &reqwest::Client::new(),
            &config,
            &[system(), user("old")],
            None,
            &cancel,
        )
        .await;
        assert!(result.is_err());
        server.join().unwrap();
    }
}

#[tokio::test]
async fn accepts_end_turn_as_a_completed_summary() {
    let (base_url, _, server) = summary_server_with(vec![(
        200,
        r#"{"choices":[{"finish_reason":"end_turn","message":{"content":"Goal: done."}}]}"#,
    )]);
    let config = AppConfig {
        base_url,
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let summary = summarize(
        &reqwest::Client::new(),
        &config,
        &[system(), user("old")],
        None,
        &cancel,
    )
    .await
    .unwrap();
    server.join().unwrap();
    assert!(summary.contains("Goal: done."));
}

#[tokio::test]
async fn summary_budget_accepts_a_prefix_above_the_main_trigger() {
    let (base_url, seen, server) = summary_server();
    let config = AppConfig {
        base_url,
        context_window: 32768,
        ..AppConfig::default()
    };
    let messages = [
        system(),
        user(&"u".repeat(52000)),
        assistant(&"a".repeat(52000)),
        user("current"),
    ];
    assert!(estimate_tokens(&messages[..3]) > crate::compaction::trigger_at(config.context_window));
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let compacted = crate::compaction::compact_messages(
        &reqwest::Client::new(),
        &config,
        &messages[1..],
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    assert!(
        compacted.summary.unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Goal: ship compaction.")
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn summary_replays_trimmed_tools_before_falling_back_to_a_digest() {
    let (base_url, seen, server) = summary_server();
    let config = AppConfig {
        base_url,
        context_window: 8192,
        ..AppConfig::default()
    };
    let messages = tool_history(5);
    let cut = messages.len() - 1;
    assert!(!prefix_fits(&messages, cut, &config, None));
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let compacted = crate::compaction::compact_at(
        &reqwest::Client::new(),
        &config,
        &messages,
        cut,
        None,
        &cancel,
        crate::compaction::CompactMode::Auto,
    )
    .await
    .unwrap();
    server.join().unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0]).unwrap();
    assert!(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .all(|message| message["content"].as_str().unwrap().chars().count() <= 4500)
    );
    assert_eq!(compacted.replaced, messages[1..cut]);
    assert!(
        compacted.summary.unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Goal: ship compaction.")
    );
}

#[test]
fn summary_replay_preserves_image_information_and_strips_internal_identity() {
    let original = json!({"role":"user", "content":[{"type":"image_url", "image_url":{"url":"data:image/png;base64,ABC"}}]});
    let replayed = summary_messages(&[crate::store::checkpoint(user("old")), original.clone()]);
    assert_eq!(replayed[1], original);
    assert!(replayed[0].get("oaklet_checkpoint").is_none());
}

#[test]
fn summary_replay_omits_oversized_image_data_urls() {
    let large = format!("data:image/png;base64,{}", "A".repeat(20_000));
    let image = json!({"role":"user", "content":[
        {"type":"text", "text":"inspect this screenshot"},
        {"type":"image_url", "image_url":{"url":large}},
    ]});
    let replayed = summary_messages(&[image]);
    let text = replayed[0].to_string();
    assert!(text.contains("image omitted"));
    assert!(!text.contains(&"A".repeat(1_000)));
    assert!(text.contains("inspect this screenshot"));
}

#[tokio::test]
async fn manual_trimmed_prefix_replay_keeps_the_latest_turn_verbatim() {
    let (base_url, _, server) = summary_server();
    let config = AppConfig {
        base_url,
        context_window: 8192,
        ..AppConfig::default()
    };
    let mut messages = tool_history(5);
    let tail = vec![
        json!({"role":"assistant", "content":"reading current", "tool_calls":[{"id":"current", "type":"function", "function":{"name":"read_file", "arguments":"{}"}}]}),
        tool("current", &"current output".repeat(1000)),
        assistant("current progress"),
    ];
    messages.extend_from_slice(&tail);
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let compacted = crate::compaction::compact_messages(
        &reqwest::Client::new(),
        &config,
        &messages[1..],
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    server.join().unwrap();
    assert_eq!(&compacted.messages[1..], tail.as_slice());
}
