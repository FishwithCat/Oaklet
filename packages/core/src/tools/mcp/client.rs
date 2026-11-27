//! MCP initialization and JSON-RPC requests shared by both transports.

mod http;
mod stdio;

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Value, json};

use self::http::HttpClient;
use self::stdio::StdioClient;
use crate::{McpServerConfig, McpTransport};

const MAX_MESSAGE_BYTES: usize = 8_000_000;
const MAX_TOOLS_PER_SERVER: usize = 500;
const MAX_LIST_PAGES: usize = 20;
const PROTOCOL_VERSION: &str = "2025-06-18";

pub(super) struct ToolSpec {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: Value,
}

pub(super) struct McpClient {
    transport: Transport,
    next_id: u64,
    protocol_version: String,
    initialized: bool,
}

enum Transport {
    Stdio(StdioClient),
    Http(HttpClient),
}

impl McpClient {
    pub(super) async fn connect(
        name: &str,
        server: &McpServerConfig,
        workspace: &Path,
    ) -> Result<Self, String> {
        let transport = match server.transport() {
            McpTransport::Stdio => Transport::Stdio(StdioClient::start(name, server, workspace)?),
            McpTransport::StreamableHttp => Transport::Http(HttpClient::new(server)?),
        };
        let mut client = Self {
            transport,
            next_id: 1,
            protocol_version: PROTOCOL_VERSION.into(),
            initialized: false,
        };
        let result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "oaklet",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            client.protocol_version = version.to_string();
        }
        client.initialized = true;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let protocol = self.protocol_version.clone();
        let initialized = self.initialized;
        match &mut self.transport {
            Transport::Stdio(stdio) => stdio.request(id, &body).await,
            Transport::Http(http) => http.request(id, &body, &protocol, initialized).await,
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let protocol = self.protocol_version.clone();
        let initialized = self.initialized;
        match &mut self.transport {
            Transport::Stdio(stdio) => stdio.write_json(&body).await,
            Transport::Http(http) => http.notify(&body, &protocol, initialized).await,
        }
    }

    pub(super) async fn list_tools(&mut self) -> Result<Vec<ToolSpec>, String> {
        let mut specs = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self.request("tools/list", params).await?;
            let tools = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or("MCP tools/list response has no tools array.".to_string())?;

            for item in tools {
                let Some(name) = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                else {
                    continue;
                };
                if !seen.insert(name.to_string()) {
                    continue;
                }
                specs.push(ToolSpec {
                    name: name.to_string(),
                    description: item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    parameters: item
                        .get("inputSchema")
                        .filter(|schema| schema.is_object())
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                });
                if specs.len() >= MAX_TOOLS_PER_SERVER {
                    break;
                }
            }

            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() || specs.len() >= MAX_TOOLS_PER_SERVER {
                break;
            }
        }

        Ok(specs)
    }

    pub(super) async fn call_tool(
        &mut self,
        name: &str,
        args: Value,
    ) -> Result<(bool, String), String> {
        let result = self
            .request("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((is_error, tool_result_text(&result)))
    }
}

fn response_result(value: Value, id: u64) -> Result<Option<Value>, String> {
    if let Value::Array(items) = value {
        for item in items {
            if let Some(result) = response_result(item, id)? {
                return Ok(Some(result));
            }
        }
        return Ok(None);
    }
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP request failed");
        return Err(match error.get("code").and_then(Value::as_i64) {
            Some(code) => format!("{message} (code {code})"),
            None => message.to_string(),
        });
    }
    Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
}

fn tool_result_text(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
                Some("image") => parts.push(format!(
                    "[image {}]",
                    block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )),
                Some("audio") => parts.push(format!(
                    "[audio {}]",
                    block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )),
                Some("resource") => {
                    let resource = block.get("resource").unwrap_or(block);
                    if let Some(text) = resource.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    } else {
                        parts.push(format!(
                            "[resource {}]",
                            resource
                                .get("uri")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                }
                Some("resource_link") => parts.push(format!(
                    "[resource link {}]",
                    block
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )),
                _ => parts.push(block.to_string()),
            }
        }
    }
    if let Some(structured) = result.get("structuredContent")
        && !structured.is_null()
    {
        parts.push(structured.to_string());
    }
    if parts.is_empty() {
        if result.is_null() {
            "No content.".into()
        } else {
            result.to_string()
        }
    } else {
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{json, response_result};

    #[test]
    fn error_messages_carry_the_server_code() {
        let error = response_result(
            json!({ "id": 3, "error": { "code": -32601, "message": "nope" } }),
            3,
        )
        .unwrap_err();
        assert_eq!(error, "nope (code -32601)");
        assert!(
            response_result(json!({ "id": 4, "result": {} }), 3)
                .unwrap()
                .is_none()
        );
    }
}
