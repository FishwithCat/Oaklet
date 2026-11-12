use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::chat::RunContext;
use crate::compaction;
use crate::context::{Meter, estimate_tokens};
use crate::repeat::RepeatGuard;
use crate::tools::{
    ApprovalDecision, ApprovalRequest, Approver, Cancel, ToolContext, ToolRegistry, Workspace, cap,
};
use crate::{AppConfig, PendingApproval, auto_approves_command};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

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
    let tools = ToolRegistry::with_host_tools(ctx.app(), &config).await;
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

    loop {
        if cancel.is_cancelled() {
            ctx.finish_stopped(messages[1..].to_vec()).await;
            return;
        }

        let pressure = meter.prepare(&messages);

        if pressure > compaction::trigger_at(config.context_window) {
            match compaction::compact(&client, &config, &messages, &tools, &cancel).await {
                Ok(Some(next)) => {
                    note_compaction(pressure, estimate_tokens(&next));
                    messages = next;
                    // Reprice the history that is actually sent, so the next
                    // usage anchor is not measured against the old size.
                    let _ = meter.prepare(&messages);
                }
                Ok(None) => {}
                Err(err) => {
                    if cancel.is_cancelled() {
                        ctx.finish_stopped(messages[1..].to_vec()).await;
                    } else {
                        ctx.finish_error(messages[1..].to_vec(), err).await;
                    }
                    return;
                }
            }
        }

        // Report the size the next request will be billed for, after any
        // compaction and while the estimate is still fresh.
        let pressure = meter.pressure(&messages);
        note_pressure(pressure);
        ctx.context(pressure, config.context_window);

        let (content, reasoning, calls, prompt_tokens) =
            match stream_step(&ctx, &client, &config, &tools, &messages, &cancel).await {
                Ok(result) => result,
                Err(err) => {
                    if cancel.is_cancelled() {
                        ctx.finish_stopped(messages[1..].to_vec()).await;
                    } else {
                        ctx.finish_error(messages[1..].to_vec(), err).await;
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

fn strip_system(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect()
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

async fn stream_step(
    ctx: &RunContext,
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &ToolRegistry,
    messages: &[Value],
    cancel: &Cancel,
) -> Result<(String, String, Vec<ToolCall>, Option<usize>), String> {
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
    let response = request
        .send()
        .await
        .map_err(|err| format!("Could not reach {}: {err}", config.base_url))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        let detail = cap(detail, 2_000);
        return Err(format!("Model request failed ({status}): {detail}"));
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
                return Err("Stopped.".into());
            }
            item = stream.next() => {
                let Some(chunk) = item else { break };
                let chunk = chunk.map_err(|err| format!("Stream interrupted: {err}"))?;
                buffer.extend_from_slice(&chunk);

                while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)?;
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
        let delta = apply_sse_line(&line, &mut content, &mut reasoning, &mut calls)?;
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

struct TauriApprover<'a> {
    ctx: &'a RunContext,
    call_id: &'a str,
    workspace: &'a Path,
    cancel: &'a Cancel,
}

impl Approver for TauriApprover<'_> {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            let allowed =
                request_approval(self.ctx, self.call_id, self.workspace, request, self.cancel)
                    .await;
            if allowed {
                ApprovalDecision::Allow
            } else if self.cancel.is_cancelled() {
                ApprovalDecision::Cancelled
            } else {
                ApprovalDecision::Deny
            }
        })
    }
}

async fn request_approval(
    ctx: &RunContext,
    call_id: &str,
    workspace: &Path,
    request: ApprovalRequest,
    cancel: &Cancel,
) -> bool {
    if cancel.is_cancelled() {
        return false;
    }

    let state = ctx.state();
    let granted = state
        .command_allow_all
        .lock()
        .ok()
        .and_then(|granted| granted.clone());
    if auto_approves_command(&request.tool, workspace, granted.as_deref()) {
        return true;
    }

    let request_id = state.next_id();
    let (sender, receiver) = oneshot::channel();
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.insert(
            request_id.clone(),
            PendingApproval {
                run_id: ctx.run_id().to_string(),
                workspace: workspace.to_path_buf(),
                sender,
            },
        );
    } else {
        return false;
    }
    if cancel.is_cancelled() {
        if let Ok(mut approvals) = state.approvals.lock() {
            approvals.remove(&request_id);
        }
        return false;
    }

    ctx.approval(
        call_id,
        &request_id,
        &request.tool,
        request.args,
        &request.prompt,
        &request.detail,
    );

    let decision = tokio::time::timeout(APPROVAL_TIMEOUT, receiver).await;
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.remove(&request_id);
    }

    let allowed = matches!(decision, Ok(Ok(true)));
    ctx.approval_resolved(call_id, allowed);
    allowed
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
        return Err(format!("Model error: {message}"));
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

fn initial_messages(
    config: &AppConfig,
    tools: &ToolRegistry,
    model_sources: &[String],
    input: Vec<Value>,
) -> Vec<Value> {
    let mut messages = Vec::with_capacity(input.len() + 1);
    messages.push(json!({
        "role": "system",
        "content": system_prompt(config, tools, model_sources)
    }));
    messages.extend(
        input
            .into_iter()
            .filter(|message| message.get("role").and_then(Value::as_str) != Some("system")),
    );
    messages
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

fn assistant_message(content: &str, reasoning: &str, calls: &[ToolCall]) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });
    // Thinking mode requires every prior turn's reasoning to be replayed while
    // tools are offered, including final answers without tool calls.
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        let calls: Vec<Value> = calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments,
                    },
                })
            })
            .collect();
        message["tool_calls"] = json!(calls);
    }
    message
}

fn tool_message(id: &str, content: String) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content,
    })
}

fn system_prompt(config: &AppConfig, tools: &ToolRegistry, model_sources: &[String]) -> String {
    let mut rules: Vec<String> = vec![
        "Use the tools to inspect and edit files. Never invent file contents or claim work you did not do.".into(),
        "Paths are relative to the workspace; absolute paths inside it also work. Shell commands start in the workspace root, so never prefix them with cd <workspace>.".into(),
    ];
    rules.extend(tools.guidance().into_iter().map(str::to_string));
    rules.extend(
        tools
            .warnings()
            .iter()
            .map(|warning| format!("MCP warning: {warning}")),
    );
    if !model_sources.is_empty() {
        rules.push(format!(
            "This workspace has LikeC4 architecture sources ({}). After you finish code changes, \
             update the affected model before your final answer so it matches the code. Read the current model \
             first and preserve its element and view IDs, style, and manual descriptions; keep the edit minimal \
             and grounded in the code you changed. Use likec4_mcp to understand existing elements and \
             relationships, and file tools to edit. If the code change has no architectural effect, leave the \
             model unchanged and say so; do not create a model when none exists.",
            model_sources.join(", ")
        ));
    }
    rules.push("Keep the final answer short: what changed and how to verify it.".into());

    format!(
        "You are Oaklet, a coding agent running locally in a desktop app. You help the user \
         inspect and change the project in their workspace.\n\n\
         Workspace: {workspace}\nPlatform: {platform}\n\n\
         Rules:\n- {rules}",
        workspace = config.workspace,
        platform = std::env::consts::OS,
        rules = rules.join("\n- "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rebuilds_the_system_prompt_and_drops_stale_ones() {
        let config = AppConfig::default();
        let tools = ToolRegistry::new();
        let messages = initial_messages(
            &config,
            &tools,
            &[],
            vec![
                json!({ "role": "system", "content": "stale prompt" }),
                json!({ "role": "user", "content": "hi" }),
            ],
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_ne!(messages[0]["content"], "stale prompt");
        assert_eq!(messages[1]["content"], "hi");
    }

    #[test]
    fn system_prompt_includes_registered_tool_guidance() {
        let prompt = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &[]);
        assert!(prompt.contains("The user must approve every command"));
        assert!(prompt.contains("never prefix them with cd <workspace>"));
        assert!(prompt.contains("Prefer edit_file"));
        assert!(prompt.contains("Read a file before editing it"));
    }

    #[test]
    fn system_prompt_includes_skill_catalog() {
        let workspace = crate::tools::test_support::temp_root("prompt-skills");
        let skill_dir = workspace.join(".oaklet/skills/review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a change.\n---\n\nRead the diff.\n",
        )
        .unwrap();

        let catalog = crate::tools::skill::SkillCatalog::discover(&workspace, None);
        let mut tools = ToolRegistry::new();
        tools.push_for_test(Box::new(crate::tools::skill::LoadSkill::new(catalog)));
        let prompt = system_prompt(&AppConfig::default(), &tools, &[]);

        assert!(prompt.contains("Available skills"));
        assert!(prompt.contains("`review`: Review a change."));
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn system_prompt_requests_architecture_updates_when_models_exist() {
        let sources = vec!["architecture/model.c4".to_string()];
        let prompt = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &sources);
        assert!(prompt.contains("architecture/model.c4"));
        assert!(prompt.contains("After you finish code changes"));
        assert!(prompt.contains("update the affected model"));

        let without_sources = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &[]);
        assert!(!without_sources.contains("LikeC4 architecture sources"));
    }

    #[test]
    fn builds_tool_messages_with_ids() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a\"}".into(),
        };
        let message = assistant_message("", "why", &[call]);
        assert_eq!(message["content"], "");
        assert_eq!(message["reasoning_content"], "why");
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            tool_message("call_1", "hi".into())["tool_call_id"],
            "call_1"
        );
    }

    #[test]
    fn final_answers_keep_reasoning_for_thinking_mode_replay() {
        let message = assistant_message("Done.", "private reasoning", &[]);
        assert_eq!(message["content"], "Done.");
        assert_eq!(message["reasoning_content"], "private reasoning");
    }
}
