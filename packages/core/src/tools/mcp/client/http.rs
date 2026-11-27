//! Streamable HTTP transport with session headers and SSE responses.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;

use super::{MAX_MESSAGE_BYTES, response_result};
use crate::McpServerConfig;
use crate::tools::{cap, web_fetch::read_limited};

pub(super) struct HttpClient {
    client: reqwest::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    session: Option<String>,
}

impl HttpClient {
    pub(super) fn new(server: &McpServerConfig) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &server.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("Invalid header name: {name}"))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| format!("Invalid value for header {name}"))?;
            headers.insert(name, value);
        }
        let client = crate::http::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| format!("Could not create MCP HTTP client: {error}"))?;
        Ok(Self {
            client,
            url: server.url.clone(),
            headers,
            session: None,
        })
    }

    async fn send(
        &mut self,
        body: &Value,
        protocol: &str,
        initialized: bool,
    ) -> Result<reqwest::Response, String> {
        let mut request = self
            .client
            .post(&self.url)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .headers(self.headers.clone())
            .json(body);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        if initialized {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("Could not reach MCP server at {}: {error}", self.url))?;
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session = Some(session.to_string());
        }
        Ok(response)
    }

    pub(super) async fn request(
        &mut self,
        id: u64,
        body: &Value,
        protocol: &str,
        initialized: bool,
    ) -> Result<Value, String> {
        let response = self.send(body, protocol, initialized).await?;
        let status = response.status();
        if !status.is_success() {
            let detail = read_limited(response, MAX_MESSAGE_BYTES)
                .await
                .unwrap_or_default();
            let detail = String::from_utf8_lossy(&detail);
            return Err(format!(
                "MCP server returned {status}: {}",
                cap(detail.into_owned(), 2_000)
            ));
        }
        if status == reqwest::StatusCode::ACCEPTED {
            return Err("MCP server accepted the request without returning a response.".into());
        }

        let is_event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if is_event_stream {
            read_sse_response(response, id).await
        } else {
            let body = read_limited(response, MAX_MESSAGE_BYTES)
                .await
                .map_err(|error| error.to_string())?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Invalid MCP response: {error}"))?;
            response_result(value, id)?.ok_or_else(|| {
                "MCP server returned a response for a different request.".to_string()
            })
        }
    }

    pub(super) async fn notify(
        &mut self,
        body: &Value,
        protocol: &str,
        initialized: bool,
    ) -> Result<(), String> {
        let response = self.send(body, protocol, initialized).await?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = read_limited(response, MAX_MESSAGE_BYTES)
                .await
                .unwrap_or_default();
            let detail = String::from_utf8_lossy(&detail);
            return Err(format!(
                "MCP server rejected notification ({status}): {}",
                cap(detail.into_owned(), 2_000)
            ));
        }
        Ok(())
    }
}

async fn read_sse_response(response: reqwest::Response, id: u64) -> Result<Value, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    loop {
        let Some(chunk) = stream.next().await else {
            return Err("MCP event stream closed without a response.".into());
        };
        let chunk = chunk.map_err(|error| format!("MCP event stream interrupted: {error}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.len() > MAX_MESSAGE_BYTES {
            return Err("MCP event stream exceeded the message limit.".into());
        }
        buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");

        while let Some(index) = buffer.find("\n\n") {
            let event = buffer[..index].to_string();
            buffer.drain(..index + 2);
            let Some(data) = sse_data(&event) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            if let Some(result) = response_result(value, id)? {
                return Ok(result);
            }
        }
    }
}

fn sse_data(event: &str) -> Option<String> {
    let mut lines = Vec::new();
    for line in event.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::sse_data;

    #[test]
    fn parses_sse_data_lines() {
        assert_eq!(
            sse_data("event: message\ndata: {\"a\":1}").as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(
            sse_data("data: one\ndata: two").as_deref(),
            Some("one\ntwo")
        );
        assert_eq!(sse_data(": keep-alive"), None);
    }
}
