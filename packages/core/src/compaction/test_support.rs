//! Shared histories, summary endpoint, and persistence assertions for compaction tests.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub(crate) fn system() -> Value {
    json!({ "role": "system", "content": "You are Oaklet." })
}

pub(crate) fn user(text: &str) -> Value {
    json!({ "role": "user", "content": text })
}

pub(crate) fn assistant(text: &str) -> Value {
    json!({ "role": "assistant", "content": text })
}

pub(super) fn tool(id: &str, text: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": id, "content": text })
}

pub(crate) const SUMMARY_BODY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"Goal: ship compaction."},"finish_reason":"stop"}]}"#;

/// OpenAI-compatible endpoint that answers each summary request with the
/// next canned response and records every request body.
pub(super) fn summary_server() -> (String, Arc<StdMutex<Vec<String>>>, thread::JoinHandle<()>) {
    summary_server_with(vec![(200, SUMMARY_BODY)])
}

pub(crate) fn summary_server_with(
    responses: Vec<(u16, &str)>,
) -> (String, Arc<StdMutex<Vec<String>>>, thread::JoinHandle<()>) {
    let responses: Vec<_> = responses
        .into_iter()
        .map(|(status, body)| (status, body.to_string()))
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let seen_by_server = seen.clone();
    let server = thread::spawn(move || {
        for (status, response_body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                if let Some(head) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&raw[..head]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if raw.len() >= head + 4 + length {
                        let request = String::from_utf8_lossy(&raw[head + 4..head + 4 + length]);
                        seen_by_server.lock().unwrap().push(request.to_string());
                        break;
                    }
                }
            }
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Bad Request"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (format!("http://{address}"), seen, server)
}

pub(crate) fn large_tool_registry() -> crate::tools::ToolRegistry {
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

    let mut tools = crate::tools::ToolRegistry::new();
    tools.push_for_test(Box::new(BigSchema("d".repeat(20_000))));
    tools
}
