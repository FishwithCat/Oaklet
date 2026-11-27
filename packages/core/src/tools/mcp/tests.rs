use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::{cache, cache_key, discover};
use crate::tools::test_support::{context, temp_root};
use crate::tools::{CancelHandle, Workspace};
use crate::{McpServerConfig, McpTransport};

fn config(command: &str, args: Vec<String>) -> McpServerConfig {
    McpServerConfig {
        kind: Some(McpTransport::Stdio),
        command: command.into(),
        args,
        enabled: true,
        ..McpServerConfig::default()
    }
}

#[tokio::test]
async fn disabled_servers_are_not_started() {
    let server = McpServerConfig {
        command: "this-command-does-not-exist".into(),
        enabled: false,
        ..McpServerConfig::default()
    };
    let servers = BTreeMap::from([("off".to_string(), server)]);
    let root = temp_root("mcp-disabled");
    let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
    let (_handle, cancel) = CancelHandle::new();
    let (tools, warnings) = discover(&servers, workspace.root(), &cancel).await;
    assert!(tools.is_empty());
    assert!(warnings.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn cancellation_interrupts_discovery_and_waiting_for_the_cache() {
    let root = temp_root("mcp-cancel-startup");
    let started = root.join("started");
    let server = config(
        "node",
        vec![
            "-e".into(),
            "require('fs').writeFileSync('started', 'ready'); process.stdin.resume();".into(),
        ],
    );
    let servers = BTreeMap::from([("slow".into(), server)]);
    let (handle, cancel) = CancelHandle::new();
    let discovery = discover(&servers, &root, &cancel);
    tokio::pin!(discovery);
    tokio::select! {
        _ = tokio::time::timeout(Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }) => assert!(started.exists(), "MCP child did not start"),
        _ = &mut discovery => panic!("unresponsive server completed discovery"),
    }
    handle.cancel();
    let (tools, warnings) = tokio::time::timeout(Duration::from_secs(1), discovery)
        .await
        .unwrap();
    assert!(tools.is_empty());
    assert!(warnings.is_empty());

    let _entries = cache().lock().await;
    let (handle, cancel) = CancelHandle::new();
    let discovery = discover(&servers, &root, &cancel);
    tokio::pin!(discovery);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(10)) => {},
        _ = &mut discovery => panic!("discovery acquired the locked cache"),
    }
    handle.cancel();
    let (tools, _) = tokio::time::timeout(Duration::from_secs(1), discovery)
        .await
        .unwrap();
    assert!(tools.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn discovers_and_calls_a_stdio_server() {
    let root = temp_root("mcp-stdio");
    let script = root.join("server.mjs");
    std::fs::write(
        &script,
        r#"
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
rl.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.method === "initialize") {
    send({ jsonrpc: "2.0", id: request.id, result: { protocolVersion: "2025-06-18", capabilities: {}, serverInfo: { name: "echo", version: "1" } } });
  } else if (request.method === "tools/list") {
    send({ jsonrpc: "2.0", id: request.id, result: { tools: [{ name: "echo", description: "Echo text", inputSchema: { type: "object", properties: { text: { type: "string" } } } }] } });
  } else if (request.method === "tools/call") {
    send({ jsonrpc: "2.0", id: request.id, result: { content: [{ type: "text", text: request.params.arguments.text }] } });
  }
});
"#,
    )
    .unwrap();

    let server = config("node", vec![script.to_string_lossy().into_owned()]);
    let servers = BTreeMap::from([("echo".to_string(), server)]);
    let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
    let (_handle, cancel) = CancelHandle::new();
    let (tools, warnings) = discover(&servers, workspace.root(), &cancel).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(tools.len(), 1);
    let (ctx, tool_root, _handle) = context("mcp-call");
    let outcome = tools[0]
        .execute(&ctx, json!({ "text": "hello" }))
        .await
        .unwrap();
    assert_eq!(outcome, "hello");
    cache()
        .lock()
        .await
        .remove(&cache_key("echo", &servers["echo"], workspace.root()));
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(tool_root).ok();
}

#[tokio::test]
async fn reuses_a_cached_stdio_server_between_runs() {
    let root = temp_root("mcp-cache");
    let started = root.join("starts.log");
    let script = root.join("server.mjs");
    std::fs::write(
        &script,
        r#"
import fs from "node:fs";
import readline from "node:readline";
fs.appendFileSync(process.env.START_LOG, "start\n");
const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
rl.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.method === "initialize") {
    send({ jsonrpc: "2.0", id: request.id, result: { protocolVersion: "2025-06-18", capabilities: {}, serverInfo: { name: "echo", version: "1" } } });
  } else if (request.method === "tools/list") {
    send({ jsonrpc: "2.0", id: request.id, result: { tools: [{ name: "echo", description: "Echo", inputSchema: { type: "object" } }] } });
  } else if (request.method === "tools/call") {
    send({ jsonrpc: "2.0", id: request.id, result: { content: [{ type: "text", text: "cached" }] } });
  }
});
"#,
    )
    .unwrap();

    let server = McpServerConfig {
        env: BTreeMap::from([(
            "START_LOG".to_string(),
            started.to_string_lossy().into_owned(),
        )]),
        ..config("node", vec![script.to_string_lossy().into_owned()])
    };
    let servers = BTreeMap::from([("echo".to_string(), server)]);
    let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
    let (_handle, cancel) = CancelHandle::new();

    let (first, warnings) = discover(&servers, workspace.root(), &cancel).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(first.len(), 1);
    let (second, warnings) = discover(&servers, workspace.root(), &cancel).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(second.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&started).unwrap().lines().count(),
        1
    );

    let (ctx, tool_root, _handle) = context("mcp-cache-call");
    let outcome = second[0].execute(&ctx, json!({})).await.unwrap();
    assert_eq!(outcome, "cached");

    cache()
        .lock()
        .await
        .remove(&cache_key("echo", &servers["echo"], workspace.root()));
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(tool_root).ok();
}

#[tokio::test]
async fn discovers_and_calls_a_streamable_http_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::<String>::new()));
    let seen_by_server = seen.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
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
                    let headers = String::from_utf8_lossy(&raw[..head]).to_string();
                    let length = headers
                        .to_ascii_lowercase()
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if raw.len() >= head + 4 + length {
                        let body =
                            String::from_utf8_lossy(&raw[head + 4..head + 4 + length]).to_string();
                        seen_by_server.lock().unwrap().push(headers);
                        if let Ok(request) = serde_json::from_str::<Value>(&body)
                            && let Some(method) = request.get("method").and_then(Value::as_str)
                        {
                            let response = match method {
                                "initialize" => Some(json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": {
                                        "protocolVersion": "2025-06-18",
                                        "capabilities": {},
                                        "serverInfo": { "name": "http", "version": "1" }
                                    }
                                })),
                                "tools/list" => Some(json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": {
                                        "tools": [{
                                            "name": "echo",
                                            "description": "Echo",
                                            "inputSchema": { "type": "object" }
                                        }]
                                    }
                                })),
                                "tools/call" => Some(json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": {
                                        "content": [{ "type": "text", "text": "over http" }]
                                    }
                                })),
                                _ => None,
                            };
                            if let Some(response) = response {
                                let body = response.to_string();
                                if method == "tools/list" {
                                    let head = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: test-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    );
                                    let _ = stream.write_all(head.as_bytes());
                                    let _ = stream.write_all(body.as_bytes());
                                } else {
                                    let event = format!("event: message\ndata: {body}\n\n");
                                    let head = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: test-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        event.len()
                                    );
                                    let _ = stream.write_all(head.as_bytes());
                                    let _ = stream.write_all(event.as_bytes());
                                }
                            } else {
                                let _ = stream.write_all(
                                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                );
                            }
                        }
                        break;
                    }
                }
            }
        }
    });

    let server_config = McpServerConfig {
        kind: Some(McpTransport::StreamableHttp),
        url: format!("http://{address}/mcp"),
        enabled: true,
        ..McpServerConfig::default()
    };
    let servers = BTreeMap::from([("http".to_string(), server_config)]);
    let root = temp_root("mcp-http");
    let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
    let (_handle, cancel) = CancelHandle::new();
    let (tools, warnings) = discover(&servers, workspace.root(), &cancel).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(tools.len(), 1);
    let (ctx, tool_root, _handle) = context("mcp-http-call");
    let outcome = tools[0].execute(&ctx, json!({})).await.unwrap();
    assert_eq!(outcome, "over http");
    server.join().unwrap();

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().any(|request| {
            request
                .to_ascii_lowercase()
                .contains("mcp-session-id: test-session")
        }));
        assert!(seen.iter().any(|request| {
            request
                .to_ascii_lowercase()
                .contains("mcp-protocol-version: 2025-06-18")
        }));
    }
    cache()
        .lock()
        .await
        .remove(&cache_key("http", &servers["http"], workspace.root()));
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(tool_root).ok();
}

#[test]
fn config_defaults_enable_the_server() {
    let servers: BTreeMap<String, McpServerConfig> =
        serde_json::from_str(r#"{"files":{}}"#).unwrap();
    let server = &servers["files"];
    assert!(server.enabled);
    assert_eq!(server.transport(), McpTransport::Stdio);
    assert!(server.env.is_empty());
}

#[test]
fn parses_the_standard_mcp_servers_json() {
    let servers: BTreeMap<String, McpServerConfig> = serde_json::from_str(
        r#"{
                "zhihu": {
                    "type": "streamableHttp",
                    "url": "https://developer.zhihu.com/api/mcp/v1",
                    "headers": { "Authorization": "Bearer secret" }
                }
            }"#,
    )
    .unwrap();
    let server = &servers["zhihu"];
    assert_eq!(server.transport(), McpTransport::StreamableHttp);
    assert_eq!(server.url, "https://developer.zhihu.com/api/mcp/v1");
    assert_eq!(server.headers["Authorization"], "Bearer secret");
    assert_eq!(
        serde_json::to_value(server).unwrap()["type"],
        "streamableHttp"
    );
}
