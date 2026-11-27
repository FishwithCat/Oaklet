//! Line-delimited JSON-RPC over a managed server process.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::{MAX_MESSAGE_BYTES, response_result};
use crate::McpServerConfig;
use crate::shell_env::ApplyEnvironment;
use crate::tools::cap;

pub(super) struct StdioClient {
    _child: Child,
    _tree: crate::process_tree::ProcessTree,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<String>>,
}

impl StdioClient {
    pub(super) fn start(
        name: &str,
        server: &McpServerConfig,
        workspace: &Path,
    ) -> Result<Self, String> {
        let launch = crate::launcher::resolve(&server.command);
        let mut command = crate::launcher::async_command(&launch.program);
        if let Some(batch) = &launch.batch {
            command.arg("/C").arg(batch);
        }
        // Server-specific environment below must win over the login shell.
        command.apply_environment();
        command
            .args(&server.args)
            .envs(&server.env)
            .current_dir(crate::paths::process_path(workspace))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "Could not start MCP server \"{}\" with {}: {error}",
                name, server.command
            )
        })?;
        let tree = crate::process_tree::ProcessTree::new(child.id());
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stderr_text = Arc::new(Mutex::new(String::new()));
        let sink = stderr_text.clone();
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4_096];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if buffer.len() < 16_384 {
                            let take = read.min(16_384 - buffer.len());
                            buffer.extend_from_slice(&chunk[..take]);
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&buffer).into_owned();
            if let Ok(mut sink) = sink.lock() {
                *sink = text;
            }
        });
        Ok(Self {
            _child: child,
            _tree: tree,
            stdin,
            stdout: BufReader::new(stdout),
            stderr: stderr_text,
        })
    }

    pub(super) async fn request(&mut self, id: u64, body: &Value) -> Result<Value, String> {
        self.write_json(body).await?;
        loop {
            let mut line = String::new();
            let read = match self.stdout.read_line(&mut line).await {
                Ok(read) => read,
                Err(error) => {
                    return Err(self.output_error(format!("Could not read MCP response: {error}")));
                }
            };
            if read == 0 {
                return Err(self.output_error("MCP server closed its output."));
            }
            if line.len() > MAX_MESSAGE_BYTES {
                return Err("MCP response exceeded the message limit.".into());
            }
            let value: Value = serde_json::from_str(line.trim())
                .map_err(|error| self.output_error(format!("Invalid MCP response: {error}")))?;

            if value.get("method").is_some() {
                if let Some(server_id) = value.get("id").cloned() {
                    let reply = server_reply(&value, server_id);
                    self.write_json(&reply).await?;
                }
                continue;
            }
            if let Some(result) = response_result(value, id)? {
                return Ok(result);
            }
        }
    }

    pub(super) async fn write_json(&mut self, body: &Value) -> Result<(), String> {
        let line = serde_json::to_string(body)
            .map_err(|error| format!("Could not encode MCP request: {error}"))?;
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("Could not write MCP request: {error}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|error| format!("Could not write MCP request: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("Could not flush MCP request: {error}"))
    }

    fn output_error(&self, message: impl Into<String>) -> String {
        let message = message.into();
        let stderr = self
            .stderr
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        if stderr.is_empty() {
            message
        } else {
            format!("{message}\nServer output: {}", cap(stderr, 2_000))
        }
    }
}

fn server_reply(request: &Value, id: Value) -> Value {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "roots/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "roots": [] }
        }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {method}") }
        }),
    }
}
