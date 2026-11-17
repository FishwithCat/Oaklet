use super::*;
use crate::compaction::test_support::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[test]
fn summary_prefix_budget_counts_the_tool_schemas_it_replays() {
    use futures_util::future::BoxFuture;

    use crate::tools::{Tool, ToolContext, ToolError, ToolOutput};

    struct BigSchema(String);
    impl Tool for BigSchema {
        fn name(&self) -> &str {
            "big_schema"
        }
        fn description(&self) -> &str {
            &self.0
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object" })
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
        ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            Box::pin(async { Ok(ToolOutput::text("")) })
        }
    }

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

    let mut tools = ToolRegistry::new();
    tools.push_for_test(Box::new(BigSchema("d".repeat(20_000))));
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
