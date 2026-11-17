use super::*;

#[tokio::test]
async fn compacts_ui_history_without_a_system_prompt() {
    use crate::tools::CancelHandle;

    let (base_url, _seen, server) = summary_server();
    let mut messages = Vec::new();
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

    let compacted = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
        .await
        .unwrap()
        .expect("compaction should apply");
    let next = compacted.messages;

    server.join().unwrap();
    assert!(!compacted.replaced.is_empty());
    assert!(
        compacted
            .replaced
            .iter()
            .all(|message| message["role"] != "system")
    );
    assert!(compacted.replaced.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("request 0 "))
    }));
    assert_eq!(next.len(), 2);
    assert!(next.iter().all(|message| message["role"] != "system"));
    let current = next[0]["content"].as_str().unwrap();
    assert!(current.contains("ship compaction."));
    assert!(current.contains("request 29"));
    assert!(!current.contains("request 0"));
    assert_eq!(next.last().unwrap(), &last);
}

#[tokio::test]
async fn manual_compaction_keeps_the_whole_latest_turn() {
    use crate::tools::CancelHandle;

    let (base_url, _seen, server) = summary_server();
    let mut messages = Vec::new();
    for step in 0..10 {
        messages.push(user(&format!("old {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user("LATEST REQUEST"));
    messages.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call_latest",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{}" }
        }]
    }));
    messages.push(tool("call_latest", &"r".repeat(800)));
    messages.push(assistant("latest answer"));

    let config = AppConfig {
        base_url,
        context_window: 8_192,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();

    let compacted = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
        .await
        .unwrap()
        .expect("compaction should apply");
    let next = compacted.messages;

    server.join().unwrap();
    assert!(
        !compacted
            .replaced
            .iter()
            .any(|message| message["content"] == "LATEST REQUEST")
    );
    assert_eq!(next.len(), 4);
    assert!(
        next[0]["content"]
            .as_str()
            .unwrap()
            .contains("LATEST REQUEST")
    );
    assert!(
        next.iter()
            .any(|message| message["tool_call_id"] == "call_latest")
    );
    assert!(
        next.iter()
            .any(|message| message["content"] == "latest answer")
    );
    assert!(!next.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("old 0"))
    }));
}

#[tokio::test]
async fn manual_compaction_skips_a_single_turn() {
    use crate::tools::CancelHandle;

    let messages = vec![user("only request"), assistant("only answer")];
    let config = AppConfig {
        context_window: 8_192,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();

    let next = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
        .await
        .unwrap();

    assert!(next.is_none());
}

#[tokio::test]
async fn manual_compaction_keeps_history_when_the_summary_fails() {
    use crate::tools::CancelHandle;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let mut messages = Vec::new();
    for step in 0..11 {
        messages.push(user(&format!("request {step} {}", "u".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    let config = AppConfig {
        base_url: format!("http://{address}"),
        context_window: 8_192,
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();

    let result = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn manual_compaction_uses_a_digest_when_the_summary_prefix_does_not_fit() {
    use crate::tools::CancelHandle;

    let mut messages = Vec::new();
    for step in 0..10 {
        messages.push(user(&format!("request {step} {}", "u".repeat(3_000))));
        messages.push(assistant(&"a".repeat(3_000)));
    }
    messages.push(user("finish the task"));
    let config = AppConfig {
        context_window: 8_192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = CancelHandle::new();
    let compacted = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    let checkpoint = compacted.messages[0]["content"].as_str().unwrap();
    assert!(checkpoint.starts_with("<compacted-summary>"));
    assert!(checkpoint.ends_with("finish the task"));
}
