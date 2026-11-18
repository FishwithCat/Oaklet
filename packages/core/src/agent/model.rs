//! One streamed chat-completion request and provider error classification.
//!
//! The coordinator owns history and retry decisions; this module turns an HTTP
//! response into emitted deltas plus accumulated content, reasoning, and tool
//! calls, and keeps provider error text classifiable as a context overflow.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};

use super::ToolCall;
use crate::AppConfig;
use crate::chat::RunContext;
use crate::tools::{Cancel, ToolRegistry, cap};

#[derive(Debug, Default)]
struct StreamDelta {
    content: Option<String>,
    reasoning: Option<String>,
    /// Provider-reported prompt size, when a chunk carries usage.
    usage: Option<usize>,
    /// Terminal marker or provider finish reason, when this chunk carries one.
    done: bool,
    finish_reason: Option<String>,
}

/// A failed model request. The status and body are kept separate so the run
/// loop can tell a context-window rejection from a missing key or a transient
/// network failure and recover only from the former.
#[derive(Debug)]
pub(super) struct RequestError {
    status: Option<reqwest::StatusCode>,
    body: String,
    /// Set when repeating the request can plausibly succeed.
    retryable: bool,
    /// Provider-requested delay before the next attempt.
    retry_after: Option<Duration>,
}

impl RequestError {
    fn response(status: reqwest::StatusCode, body: String) -> Self {
        Self {
            status: Some(status),
            body,
            retryable: false,
            retry_after: None,
        }
    }

    fn response_retry(
        status: reqwest::StatusCode,
        body: String,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            status: Some(status),
            body,
            retryable: true,
            retry_after,
        }
    }

    fn stream(body: String) -> Self {
        Self {
            status: None,
            body,
            retryable: false,
            retry_after: None,
        }
    }

    fn transport(body: String) -> Self {
        Self {
            status: None,
            body,
            retryable: true,
            retry_after: None,
        }
    }

    /// Whether the run loop should send the same request again. Only failures
    /// before any streamed content are safe to repeat: replaying a partial
    /// reply would duplicate text the user already saw.
    pub(super) fn is_transient(&self) -> bool {
        self.retryable
    }

    /// Honor `Retry-After` when present, otherwise back off exponentially.
    pub(super) fn retry_delay(&self, attempt: usize) -> Duration {
        let backoff = Duration::from_secs(1 << attempt.min(4));
        self.retry_after
            .map(|delay| delay.min(Duration::from_secs(30)))
            .unwrap_or(backoff)
    }

    pub(super) fn is_context_overflow(&self) -> bool {
        // Auth and quota failures are not fixed by shrinking the prompt.
        if self
            .status
            .is_some_and(|status| matches!(status.as_u16(), 401..=403))
        {
            return false;
        }
        // Rate limits often mention tokens without saying anything about the
        // prompt size; compacting for them would only add another request.
        if self.status.is_some_and(|status| status.as_u16() == 429) {
            return self.body.to_ascii_lowercase().contains("context")
                && is_context_overflow(&self.body);
        }
        is_context_overflow(&self.body)
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(formatter, "Model request failed ({status}): {}", self.body),
            None => formatter.write_str(&self.body),
        }
    }
}

impl std::error::Error for RequestError {}

/// Provider text that means the prompt itself was too large. Providers word
/// this differently, so require either an explicit context phrase with an
/// exceed verb, or wording that directly names the model context bound. A miss
/// is safe: the run reports the original provider error instead of compacting.
fn is_context_overflow(body: &str) -> bool {
    let text = body.to_ascii_lowercase();
    if [
        "rate limit",
        "too many requests",
        "per minute",
        "quota exceeded",
        "insufficient balance",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
    {
        return false;
    }

    // Provider codes and prose use different separators; normalize both so
    // `context_length_exceeded` and `context-window-overflowed` read the same.
    let normalized: String = text
        .chars()
        .map(|character| match character {
            '_' | '-' => ' ',
            other => other,
        })
        .collect();
    let mentions_context = normalized.contains("context");
    let exceeds = normalized.contains("exceed")
        || normalized.contains("overflow")
        || normalized.contains("too long")
        || normalized.contains("too large")
        || normalized.contains("larger than");

    // Structured errors such as `context_length_exceeded`, plus prose that
    // names which context bound was crossed.
    if (normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("context limit")
        || normalized.contains("maximum context")
        || normalized.contains("max context"))
        && exceeds
    {
        return true;
    }

    // "This model's maximum context length is 128000 tokens" names the bound
    // itself and does not need an exceed verb.
    if normalized.contains("maximum context length")
        || normalized.contains("maximum context window")
        || normalized.contains("maximum allowed context")
        || normalized.contains("maximum supported context")
        || normalized.contains("max context length")
        || normalized.contains("max context window")
        || normalized.contains("max allowed context")
        || normalized.contains("max supported context")
    {
        return true;
    }

    // Request-size wording tied directly to model context.
    if (normalized.contains("too long") || normalized.contains("too large"))
        && mentions_context
        && (normalized.contains("request")
            || normalized.contains("prompt")
            || normalized.contains("input")
            || normalized.contains("message"))
    {
        return true;
    }
    if mentions_context
        && exceeds
        && (normalized.contains("input")
            || normalized.contains("prompt")
            || normalized.contains("request")
            || normalized.contains("message"))
    {
        return true;
    }

    // Anthropic says the prompt is too long without naming the context bound.
    if normalized.contains("prompt is too long")
        || normalized.contains("prompt too long")
        || normalized.contains("input is too long")
        || normalized.contains("input too long")
    {
        return true;
    }

    // vLLM, Ollama, and Mistral wording.
    if normalized.contains("maximum sequence length")
        || normalized.contains("max seq len")
        || (normalized.contains("maximum number of tokens")
            && (mentions_context || normalized.contains("input") || normalized.contains("prompt")))
        || normalized.contains("max input tokens")
        || normalized.contains("too many tokens")
        || normalized.contains("too many total tokens")
        || (normalized.contains("input length")
            && (normalized.contains("exceed") || normalized.contains("larger than")))
    {
        return true;
    }

    text.contains("上下文长度") || text.contains("上下文窗口")
}

/// Statuses where the same request may succeed after a short wait. A 4xx
/// other than a timeout or rate limit describes the request itself and will
/// fail the same way on every attempt.
fn is_transient_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

/// `Retry-After` in delta-seconds form. HTTP-date values are ignored; the
/// exponential fallback is still a safe delay.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

pub(super) async fn stream_step(
    ctx: &RunContext,
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &ToolRegistry,
    messages: &[Value],
    cancel: &Cancel,
) -> Result<(String, String, Vec<ToolCall>, Option<usize>), RequestError> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let body = json!({
        "model": config.model,
        "messages": crate::context::request_messages(messages),
        "tools": tools.schemas(),
        "stream": true,
        "max_tokens": crate::context::output_reserve(config.context_window),
        // Ask OpenAI-compatible servers to close the stream with a usage chunk.
        "stream_options": { "include_usage": true },
    });

    let mut request = client.post(&url).json(&body);
    if !config.api_key.is_empty() {
        request = request.bearer_auth(&config.api_key);
    }
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(RequestError::stream("Stopped.".into())),
        response = request.send() => response.map_err(|err| {
            RequestError::transport(format!("Could not reach {}: {err}", config.base_url))
        })?,
    };

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let detail = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(RequestError::stream("Stopped.".into())),
            detail = response.text() => detail.unwrap_or_default(),
        };
        let detail = cap(detail, 2_000);
        if is_transient_status(status) {
            return Err(RequestError::response_retry(status, detail, retry_after));
        }
        return Err(RequestError::response(status, detail));
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
    let mut prompt_tokens: Option<usize> = None;
    let mut saw_done = false;
    let mut finish_reason: Option<String> = None;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(RequestError::stream("Stopped.".into()));
            }
            item = stream.next() => {
                let Some(chunk) = item else { break };
                let chunk = chunk
                    .map_err(|err| RequestError::stream(format!("Stream interrupted: {err}")))?;
                buffer.extend_from_slice(&chunk);

                while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let mut delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)
                        .map_err(RequestError::stream)?;
                    if let Some(tokens) = delta.usage {
                        prompt_tokens = Some(tokens);
                    }
                    saw_done |= delta.done;
                    if let Some(reason) = delta.finish_reason.take() {
                        finish_reason = Some(reason);
                    }
                    emit_stream_delta(ctx, delta);
                }
            }
        }
    }

    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer);
        let mut delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)
            .map_err(RequestError::stream)?;
        if let Some(tokens) = delta.usage {
            prompt_tokens = Some(tokens);
        }
        saw_done |= delta.done;
        if let Some(reason) = delta.finish_reason.take() {
            finish_reason = Some(reason);
        }
        emit_stream_delta(ctx, delta);
    }

    if let Some(message) = stream_end_error(saw_done, finish_reason.as_deref()) {
        return Err(RequestError::stream(message.into()));
    }

    let mut calls: Vec<ToolCall> = calls.into_values().collect();
    assign_call_ids(&mut calls, &ctx.state().next_id());
    Ok((content, reasoning, calls, prompt_tokens))
}

fn assign_call_ids(calls: &mut [ToolCall], response_id: &str) {
    // Providers may omit or reuse IDs; storage and replay need unique identities.
    for (index, call) in calls.iter_mut().enumerate() {
        call.id = format!("call_{response_id}_{index}");
    }
}

fn stream_end_error(saw_done: bool, finish_reason: Option<&str>) -> Option<&'static str> {
    if finish_reason == Some("length") {
        Some("The model reached its output limit before finishing.")
    } else if !saw_done && finish_reason.is_none() {
        Some("The model stream ended before completion.")
    } else {
        None
    }
}

fn emit_stream_delta(ctx: &RunContext, delta: StreamDelta) {
    if let Some(text) = delta.reasoning {
        ctx.reasoning(&text);
    }
    if let Some(text) = delta.content {
        ctx.delta(&text);
    }
}

fn apply_sse_line(
    line: &str,
    content: &mut String,
    reasoning: &mut String,
    calls: &mut BTreeMap<usize, ToolCall>,
) -> Result<StreamDelta, String> {
    let Some(data) = line.trim_end().strip_prefix("data:") else {
        return Ok(StreamDelta::default());
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(StreamDelta::default());
    }
    if data == "[DONE]" {
        return Ok(StreamDelta {
            done: true,
            ..StreamDelta::default()
        });
    }

    let value: Value =
        serde_json::from_str(data).map_err(|err| format!("Malformed stream chunk: {err}"))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        // Some providers put the useful classification in `code` and leave the
        // message generic, so keep it in the body the overflow matcher sees.
        let body = match error
            .get("code")
            .or_else(|| error.get("type"))
            .and_then(Value::as_str)
        {
            Some(code) if !message.contains(code) => format!("{message} ({code})"),
            _ => message,
        };
        return Err(format!("Model error: {body}"));
    }

    let mut emitted = StreamDelta::default();
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        emitted.finish_reason = Some(reason.to_string());
    }
    if let Some(tokens) = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
    {
        emitted.usage = Some(tokens as usize);
    }

    let Some(delta) = value.pointer("/choices/0/delta") else {
        return Ok(emitted);
    };

    if let Some(text) = delta.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push_str(text);
        emitted.content = Some(text.to_string());
    }

    // DeepSeek-compatible chat completions stream reasoning under
    // `reasoning_content`.
    if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
        && !text.is_empty()
    {
        reasoning.push_str(text);
        emitted.reasoning = Some(text.to_string());
    }

    if let Some(items) = delta.get("tool_calls").and_then(Value::as_array) {
        for item in items {
            let index = item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let call = calls.entry(index).or_default();
            if let Some(id) = item.get("id").and_then(Value::as_str)
                && !id.is_empty()
            {
                call.id = id.to_string();
            }
            if let Some(name) = item.pointer("/function/name").and_then(Value::as_str)
                && !name.is_empty()
            {
                call.name.push_str(name);
            }
            if let Some(args) = item.pointer("/function/arguments").and_then(Value::as_str) {
                call.arguments.push_str(args);
            }
        }
    }

    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_repeated_provider_ids_are_unique_in_history_and_results() {
        let mut calls = vec![
            ToolCall::default(),
            ToolCall {
                id: "repeated".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "repeated".into(),
                ..ToolCall::default()
            },
        ];
        assign_call_ids(&mut calls, "response-1");
        let mut later = vec![ToolCall {
            id: "repeated".into(),
            ..ToolCall::default()
        }];
        assign_call_ids(&mut later, "response-2");
        let ids: std::collections::HashSet<_> =
            calls.iter().chain(&later).map(|call| &call.id).collect();
        assert_eq!(ids.len(), 4);
        let assistant = super::super::messages::assistant_message("", "", &calls);
        for (index, call) in calls.iter().enumerate() {
            let result = super::super::messages::tool_message(&call.id, "output".into());
            assert_eq!(assistant["tool_calls"][index]["id"], result["tool_call_id"]);
        }
        assert_eq!(calls[1].name, "read_file");
        assert_eq!(calls[1].arguments, "{}");
    }

    #[test]
    fn recognizes_context_window_rejections() {
        for body in [
            "This model's maximum context length is 8192 tokens.",
            "This model maximum context length is 128000 tokens",
            "maximum context exceeded",
            "context-window-overflowed",
            "prompt is too long: 210399 tokens > 200000 maximum",
            "input is too long for this model",
            "request too large for model context",
            "input exceeds the model context window limit",
            "Too many tokens",
            "The input token count exceeds the maximum number of tokens allowed",
            "context_length_exceeded",
            "上下文长度超出限制",
        ] {
            assert!(is_context_overflow(body), "{body}");
        }

        for body in [
            "Invalid API key",
            "Rate limit reached for requests",
            "Too many requests, retry after 10s",
            "Connection reset by peer",
            "invalid request: malformed tool arguments",
            "invalid input: temperature exceeds maximum allowed value",
            "input exceeds maximum allowed value",
            "context window size must be positive",
            "the model's maximum output tokens is 4096",
        ] {
            assert!(!is_context_overflow(body), "{body}");
        }
    }

    #[test]
    fn rate_limits_are_not_context_recoveries() {
        let error = RequestError::response(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "too many tokens per minute".into(),
        );
        assert!(!error.is_context_overflow());

        let error = RequestError::response(
            reqwest::StatusCode::UNAUTHORIZED,
            "maximum context length".into(),
        );
        assert!(!error.is_context_overflow());

        let error = RequestError::response(
            reqwest::StatusCode::BAD_REQUEST,
            "maximum context length is 8192 tokens".into(),
        );
        assert!(error.is_context_overflow());
    }

    #[test]
    fn retries_only_transient_provider_failures() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                RequestError::response_retry(status, "later".into(), None).is_transient(),
                "{status}"
            );
        }
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::NOT_FOUND,
        ] {
            assert!(!RequestError::response(status, "no".into()).is_transient());
        }
        assert!(RequestError::transport("offline".into()).is_transient());
        // A stream that broke after emitting content must not replay it.
        assert!(!RequestError::stream("interrupted".into()).is_transient());
    }

    #[test]
    fn retry_delay_honors_retry_after_and_backs_off() {
        let error = RequestError::response_retry(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "slow down".into(),
            Some(Duration::from_secs(7)),
        );
        assert_eq!(error.retry_delay(0), Duration::from_secs(7));

        let error = RequestError::response_retry(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "slow down".into(),
            Some(Duration::from_secs(600)),
        );
        assert_eq!(error.retry_delay(0), Duration::from_secs(30));

        let error = RequestError::transport("offline".into());
        assert_eq!(error.retry_delay(0), Duration::from_secs(1));
        assert_eq!(error.retry_delay(1), Duration::from_secs(2));
    }

    #[test]
    fn parses_delta_seconds_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(12)));

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn accumulates_content_reasoning_and_streamed_tool_calls() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = BTreeMap::new();

        let delta = apply_sse_line(
            r#"data: {"choices":[{"delta":{"content":"Let me look."}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.content.as_deref(), Some("Let me look."));
        assert_eq!(delta.reasoning, None);

        let delta = apply_sse_line(
            r#"data: {"choices":[{"delta":{"reasoning_content":"Check the path first.\n"}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.reasoning.as_deref(), Some("Check the path first.\n"));

        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"src\"}"}}]}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();

        assert_eq!(content, "Let me look.");
        assert_eq!(reasoning, "Check the path first.\n");
        let calls: Vec<ToolCall> = calls.into_values().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"src"}"#);
    }

    #[test]
    fn ignores_comments_done_and_surfaces_model_errors() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = BTreeMap::new();
        let delta =
            apply_sse_line(": keep-alive", &mut content, &mut reasoning, &mut calls).unwrap();
        assert!(delta.content.is_none() && delta.reasoning.is_none());
        let delta =
            apply_sse_line("data: [DONE]", &mut content, &mut reasoning, &mut calls).unwrap();
        assert!(delta.content.is_none() && delta.reasoning.is_none());
        assert!(delta.done);
        let delta = apply_sse_line(
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.finish_reason.as_deref(), Some("stop"));
        assert!(!delta.done);
        assert!(
            apply_sse_line(
                r#"data: {"error":{"message":"bad key"}}"#,
                &mut content,
                &mut reasoning,
                &mut calls
            )
            .unwrap_err()
            .contains("bad key")
        );
        assert!(
            apply_sse_line(
                r#"data: {"error":{"message":"request failed","code":"context_length_exceeded"}}"#,
                &mut content,
                &mut reasoning,
                &mut calls
            )
            .unwrap_err()
            .contains("context_length_exceeded")
        );
    }

    #[test]
    fn reads_prompt_tokens_from_a_usage_chunk() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = BTreeMap::new();

        let delta = apply_sse_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":1234,"completion_tokens":5}}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.usage, Some(1234));

        let delta = apply_sse_line(
            r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.usage, None);
    }

    #[test]
    fn rejects_a_stream_without_a_terminal_marker() {
        assert!(stream_end_error(false, None).is_some());
        assert!(stream_end_error(false, Some("length")).is_some());
        assert!(stream_end_error(false, Some("stop")).is_none());
        assert!(stream_end_error(true, None).is_none());
        assert!(stream_end_error(true, Some("tool_calls")).is_none());
    }

    #[test]
    fn rejects_malformed_stream_json() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = BTreeMap::new();

        let error = apply_sse_line("data: {not json", &mut content, &mut reasoning, &mut calls)
            .unwrap_err();
        assert!(error.contains("Malformed stream chunk"), "{error}");
        assert!(content.is_empty() && reasoning.is_empty() && calls.is_empty());
    }

    #[test]
    fn orders_multiple_tool_call_indices_and_appends_fragmented_names() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = BTreeMap::new();

        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"write_","arguments":"{\"pa"}},{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{}"}}]}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();
        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"file","arguments":"th\":\"src\"}"}}]}}]}"#,
            &mut content,
            &mut reasoning,
            &mut calls,
        )
        .unwrap();

        let calls: Vec<ToolCall> = calls.into_values().collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].name, "write_file");
        assert_eq!(calls[1].arguments, r#"{"path":"src"}"#);
    }
}
