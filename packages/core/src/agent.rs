mod approval;
mod messages;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::AppConfig;
use crate::chat::RunContext;
use crate::compaction;
use crate::context::{Meter, estimate_tokens};
use crate::repeat::RepeatGuard;
use crate::tools::{Cancel, ToolContext, ToolRegistry, Workspace, cap};

use approval::TauriApprover;
use messages::{assistant_message, initial_messages, strip_system, tool_message};

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
struct StreamDelta {
    content: Option<String>,
    reasoning: Option<String>,
    /// Provider-reported prompt size, when a chunk carries usage.
    usage: Option<usize>,
}

/// A failed model request. The status and body are kept separate so the run
/// loop can tell a context-window rejection from a missing key or a transient
/// network failure and recover only from the former.
#[derive(Debug)]
pub(crate) struct RequestError {
    pub(crate) status: Option<reqwest::StatusCode>,
    pub(crate) body: String,
}

impl RequestError {
    pub(crate) fn response(status: reqwest::StatusCode, body: String) -> Self {
        Self {
            status: Some(status),
            body,
        }
    }

    pub(crate) fn stream(body: String) -> Self {
        Self { status: None, body }
    }

    pub(crate) fn is_context_overflow(&self) -> bool {
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

/// One turn. The backend already persisted the user message and registered the
/// run; this loop streams, executes tools, and finishes through `ctx`, which
/// writes the transcript and publishes the terminal event.
pub async fn run(config: AppConfig, input: Vec<Value>, cancel: Cancel, ctx: RunContext) {
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            ctx.finish_error(
                strip_system(&input),
                format!("Could not create HTTP client: {err}"),
            )
            .await;
            return;
        }
    };

    let workspace = match Workspace::new(&config.workspace) {
        Ok(workspace) => workspace,
        Err(message) => {
            ctx.finish_error(strip_system(&input), message).await;
            return;
        }
    };
    let mut tools = ToolRegistry::with_host_tools(ctx.app(), &config).await;
    ensure_history_tool(&mut tools, &ctx);
    let tool_context = ToolContext {
        workspace,
        cancel: cancel.clone(),
        run_id: ctx.run_id().to_string(),
    };

    let model_sources = {
        let workspace = config.workspace.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::architecture::prompt_model_sources(Path::new(&workspace))
        })
        .await
        .unwrap_or_default()
    };

    let mut messages = initial_messages(&config, &tools, &model_sources, input);
    let mut meter = Meter::default();
    let mut guard = RepeatGuard::new();
    // A forced compaction already shrank the prompt for the retry, so do not
    // immediately run the normal preflight on it again.
    let mut retry_directly = false;
    // Only one forced retry per step; a second rejection reports the error.
    let mut overflow_retried = false;

    loop {
        if cancel.is_cancelled() {
            ctx.finish_stopped(messages[1..].to_vec()).await;
            return;
        }

        // This also runs before the first request of a run, so a saved history
        // that no longer fits the configured window after a model switch is
        // compacted before it is sent.
        if !retry_directly {
            let mut replaced = Vec::new();
            let outcome = compact_before_request(
                &client,
                &config,
                &tools,
                &cancel,
                &mut messages,
                &mut meter,
                &mut replaced,
            )
            .await;
            if !replaced.is_empty() {
                ctx.record_archived(&replaced);
                ensure_history_tool(&mut tools, &ctx);
            }
            if let Err(err) = outcome {
                if cancel.is_cancelled() {
                    ctx.finish_stopped(messages[1..].to_vec()).await;
                } else {
                    ctx.finish_error(messages[1..].to_vec(), err).await;
                }
                return;
            }
        }
        retry_directly = false;

        // Report the size the next request will be billed for, after any
        // compaction and while the estimate is still fresh.
        let pressure = meter.pressure(&messages);
        note_pressure(pressure);
        ctx.context(pressure, config.context_window);

        let (content, reasoning, calls, prompt_tokens) =
            match stream_step(&ctx, &client, &config, &tools, &messages, &cancel).await {
                Ok(result) => {
                    overflow_retried = false;
                    result
                }
                Err(err) if err.is_context_overflow() && !overflow_retried => {
                    overflow_retried = true;
                    match compaction::force(&client, &config, &messages, &tools, &cancel).await {
                        Ok(Some(compacted)) => {
                            note_compaction(
                                estimate_tokens(&messages),
                                estimate_tokens(&compacted.messages),
                            );
                            ctx.record_archived(&compacted.replaced);
                            ensure_history_tool(&mut tools, &ctx);
                            messages = compacted.messages;
                            // The retry is a different, smaller request. Price
                            // it before the next usage anchor is recorded.
                            let _ = meter.prepare(&messages);
                            retry_directly = true;
                            continue;
                        }
                        Ok(None) => {
                            ctx.finish_error(
                                messages[1..].to_vec(),
                                format!(
                                    "{err}\n\nThe conversation could not be compacted further; \
                                     start a new chat or raise the context window."
                                ),
                            )
                            .await;
                            return;
                        }
                        Err(force_err) => {
                            if cancel.is_cancelled() {
                                ctx.finish_stopped(messages[1..].to_vec()).await;
                            } else {
                                ctx.finish_error(messages[1..].to_vec(), force_err).await;
                            }
                            return;
                        }
                    }
                }
                Err(err) => {
                    if cancel.is_cancelled() {
                        ctx.finish_stopped(messages[1..].to_vec()).await;
                    } else {
                        ctx.finish_error(messages[1..].to_vec(), err.to_string())
                            .await;
                    }
                    return;
                }
            };
        if let Some(prompt_tokens) = prompt_tokens {
            meter.record_prompt_tokens(prompt_tokens);
        }

        if content.trim().is_empty() && calls.is_empty() {
            ctx.finish_error(
                messages[1..].to_vec(),
                "The model returned an empty response.",
            )
            .await;
            return;
        }

        messages.push(assistant_message(&content, &reasoning, &calls));
        ctx.context(meter.pressure(&messages), config.context_window);
        if calls.is_empty() {
            // The system prompt is rebuilt every turn; keep it out of the
            // conversation that is saved.
            ctx.finish_done(messages[1..].to_vec()).await;
            return;
        }

        // Reminders wait until every result in this batch exists, so a user
        // message never splits the tool results answering one assistant turn.
        let mut reminders = Vec::new();

        for call in &calls {
            if cancel.is_cancelled() {
                ctx.finish_stopped(messages[1..].to_vec()).await;
                return;
            }

            let args = match parse_args(&call.arguments) {
                Ok(args) => args,
                Err(message) => {
                    ctx.tool_start(&call.id, &call.name, "");
                    ctx.tool_result(&call.id, false, &message);
                    if let Some(reminder) =
                        guard.observe(&call.name, &Value::String(call.arguments.clone()))
                    {
                        reminders.push(reminder);
                    }
                    messages.push(tool_message(&call.id, message));
                    continue;
                }
            };

            let summary = tools.summary(&call.name, &args);
            ctx.tool_start(&call.id, &call.name, &summary);

            let approver = TauriApprover {
                ctx: &ctx,
                call_id: &call.id,
                workspace: tool_context.workspace.root(),
                cancel: &cancel,
            };
            if let Some(reminder) = guard.observe(&call.name, &args) {
                reminders.push(reminder);
            }
            let outcome = tools
                .execute(&tool_context, &approver, &call.name, &call.id, args)
                .await;
            ctx.tool_result(&call.id, outcome.ok, &outcome.output);
            messages.push(tool_message(&call.id, outcome.output));

            if cancel.is_cancelled() {
                ctx.finish_stopped(messages[1..].to_vec()).await;
                return;
            }
        }

        for reminder in reminders {
            messages.push(json!({ "role": "user", "content": reminder }));
        }
    }
}

/// Print context pressure while developing; release builds stay quiet.
fn note_pressure(pressure: usize) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] context pressure ~{pressure} tokens");
    }
}

fn note_compaction(before: usize, after: usize) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] compacted context ~{before} -> ~{after} tokens");
    }
}

/// Keep the next request under the configured window.
///
/// Normal compaction runs first so a run that is merely near the limit keeps a
/// comfortable recent tail. If that still leaves the prompt over the trigger, or
/// there is nothing old enough for the normal cut, force a deeper pass before
/// sending. This is also the first thing a run does, which covers a saved
/// history that no longer fits after the user switches models or windows.
async fn compact_before_request(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &ToolRegistry,
    cancel: &Cancel,
    messages: &mut Vec<Value>,
    meter: &mut Meter,
    archived: &mut Vec<Value>,
) -> Result<(), String> {
    let trigger = compaction::trigger_at(config.context_window);
    let pressure = meter.prepare(messages);
    if pressure <= trigger {
        return Ok(());
    }

    if let Some(compacted) = compaction::compact(client, config, messages, tools, cancel).await? {
        note_compaction(pressure, estimate_tokens(&compacted.messages));
        archived.extend(compacted.replaced);
        *messages = compacted.messages;
        // Reprice the history that is actually sent, so the next usage anchor
        // is not measured against the old size.
        let _ = meter.prepare(messages);
    }

    // A normal compaction keeps a larger tail and refuses a history with
    // nothing old enough to replace. If the request is still over the trigger,
    // make one forced pass with a much smaller retain before sending it.
    if meter.pressure(messages) > trigger
        && let Some(compacted) = compaction::force(client, config, messages, tools, cancel).await?
    {
        note_compaction(
            estimate_tokens(messages),
            estimate_tokens(&compacted.messages),
        );
        archived.extend(compacted.replaced);
        *messages = compacted.messages;
        let _ = meter.prepare(messages);
    }

    Ok(())
}

/// Add `history_read` only once this chat has archived originals to read.
fn ensure_history_tool(tools: &mut ToolRegistry, ctx: &RunContext) {
    if tools.get("history_read").is_some() {
        return;
    }
    let tool = crate::tools::history::HistoryRead::new(
        ctx.store(),
        ctx.session_id().to_string(),
        ctx.archived(),
    );
    if tool.has_entries() {
        tools.push(Box::new(tool));
    }
}

async fn stream_step(
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
        "messages": messages,
        "tools": tools.schemas(),
        "stream": true,
        // Ask OpenAI-compatible servers to close the stream with a usage chunk.
        "stream_options": { "include_usage": true },
    });

    let mut request = client.post(&url).json(&body);
    if !config.api_key.is_empty() {
        request = request.bearer_auth(&config.api_key);
    }
    let response = request.send().await.map_err(|err| {
        RequestError::stream(format!("Could not reach {}: {err}", config.base_url))
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        let detail = cap(detail, 2_000);
        return Err(RequestError::response(status, detail));
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
    let mut prompt_tokens: Option<usize> = None;

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
                    let delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)
                        .map_err(RequestError::stream)?;
                    if let Some(tokens) = delta.usage {
                        prompt_tokens = Some(tokens);
                    }
                    emit_stream_delta(ctx, delta);
                }
            }
        }
    }

    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer);
        let delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)
            .map_err(RequestError::stream)?;
        if let Some(tokens) = delta.usage {
            prompt_tokens = Some(tokens);
        }
        emit_stream_delta(ctx, delta);
    }

    let mut calls: Vec<ToolCall> = calls.into_values().collect();
    for (index, call) in calls.iter_mut().enumerate() {
        if call.id.is_empty() {
            call.id = format!("call_{index}");
        }
    }
    Ok((content, reasoning, calls, prompt_tokens))
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
    if data.is_empty() || data == "[DONE]" {
        return Ok(StreamDelta::default());
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

fn parse_args(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("Invalid tool arguments: {err}"))?;
    if !value.is_object() {
        return Err("Tool arguments must be a JSON object.".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn preflight_compacts_saved_history_before_the_first_request() {
        use crate::tools::CancelHandle;

        let mut messages = vec![json!({
            "role": "system",
            "content": "You are Oaklet.",
        })];
        for step in 0..40 {
            messages.push(json!({
                "role": "user",
                "content": format!("request {step} {}", "u".repeat(800)),
            }));
            messages.push(json!({
                "role": "assistant",
                "content": "a".repeat(800),
            }));
        }
        let before = estimate_tokens(&messages);
        let config = AppConfig {
            // The summary request fails, so the preflight must fall back to
            // dropping the old range just as a provider-limited run would.
            base_url: "http://127.0.0.1:1".into(),
            context_window: 8_192,
            ..AppConfig::default()
        };
        let mut meter = Meter::default();
        let mut archived = Vec::new();
        let (_handle, cancel) = CancelHandle::new();

        compact_before_request(
            &reqwest::Client::new(),
            &config,
            &ToolRegistry::new(),
            &cancel,
            &mut messages,
            &mut meter,
            &mut archived,
        )
        .await
        .unwrap();

        assert!(archived.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("request 0"))
        }));
        let after = estimate_tokens(&messages);
        assert!(after < before);
        assert!(after <= compaction::trigger_at(config.context_window));
        assert!(messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("dropped so the run could stay"))
        }));
    }

    #[test]
    fn parses_empty_and_invalid_arguments() {
        assert_eq!(parse_args("").unwrap(), json!({}));
        assert!(parse_args("not json").is_err());
        assert!(parse_args("[1,2]").is_err());
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
}
