use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::{AppConfig, PendingApproval, SharedState, tools};

const MAX_STEPS: usize = 32;
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TOOL_RESULT_CHARS: usize = 30_000;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaEvent {
    run_id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolStartEvent {
    run_id: String,
    id: String,
    name: String,
    args: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultEvent {
    run_id: String,
    id: String,
    ok: bool,
    output: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalEvent {
    run_id: String,
    id: String,
    request_id: String,
    tool: String,
    args: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    run_id: String,
    messages: Vec<Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunIdEvent {
    run_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    run_id: String,
    message: String,
}

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: T) {
    let _ = app.emit(event, payload);
}

fn fail(app: &AppHandle, run_id: &str, message: impl Into<String>) {
    emit(
        app,
        "agent:error",
        ErrorEvent {
            run_id: run_id.to_string(),
            message: message.into(),
        },
    );
}

fn stopped(app: &AppHandle, run_id: &str) {
    emit(
        app,
        "agent:stopped",
        RunIdEvent {
            run_id: run_id.to_string(),
        },
    );
}

pub async fn run(
    app: AppHandle,
    state: SharedState,
    run_id: String,
    input: Vec<Value>,
    config: AppConfig,
    cancel: Arc<AtomicBool>,
) {
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            fail(
                &app,
                &run_id,
                format!("Could not create HTTP client: {err}"),
            );
            return;
        }
    };

    let mut messages = initial_messages(&config, input);

    for _ in 0..MAX_STEPS {
        if cancel.load(Ordering::SeqCst) {
            stopped(&app, &run_id);
            return;
        }

        let (content, calls) =
            match stream_step(&app, &run_id, &client, &config, &messages, &cancel).await {
                Ok(result) => result,
                Err(err) => {
                    if cancel.load(Ordering::SeqCst) {
                        stopped(&app, &run_id);
                    } else {
                        fail(&app, &run_id, err);
                    }
                    return;
                }
            };

        if content.trim().is_empty() && calls.is_empty() {
            fail(&app, &run_id, "The model returned an empty response.");
            return;
        }

        messages.push(assistant_message(&content, &calls));
        if calls.is_empty() {
            emit(
                &app,
                "agent:done",
                DoneEvent {
                    run_id,
                    // The system prompt is rebuilt every turn; keep it out of
                    // the conversation the UI sends back.
                    messages: messages[1..].to_vec(),
                },
            );
            return;
        }

        for call in &calls {
            if cancel.load(Ordering::SeqCst) {
                stopped(&app, &run_id);
                return;
            }

            let args = match parse_args(&call.arguments) {
                Ok(args) => args,
                Err(message) => {
                    emit(
                        &app,
                        "agent:tool_start",
                        ToolStartEvent {
                            run_id: run_id.clone(),
                            id: call.id.clone(),
                            name: call.name.clone(),
                            args: Value::Null,
                        },
                    );
                    emit(
                        &app,
                        "agent:tool_result",
                        ToolResultEvent {
                            run_id: run_id.clone(),
                            id: call.id.clone(),
                            ok: false,
                            output: message.clone(),
                        },
                    );
                    messages.push(tool_message(&call.id, message));
                    continue;
                }
            };

            emit(
                &app,
                "agent:tool_start",
                ToolStartEvent {
                    run_id: run_id.clone(),
                    id: call.id.clone(),
                    name: call.name.clone(),
                    args: args.clone(),
                },
            );

            if call.name == "run_command"
                && !request_approval(&app, &state, &run_id, &call.id, &args, &cancel).await
            {
                let output = if cancel.load(Ordering::SeqCst) {
                    "Command cancelled.".to_string()
                } else {
                    "The user denied this command.".to_string()
                };
                emit(
                    &app,
                    "agent:tool_result",
                    ToolResultEvent {
                        run_id: run_id.clone(),
                        id: call.id.clone(),
                        ok: false,
                        output: output.clone(),
                    },
                );
                messages.push(tool_message(&call.id, output));
                if cancel.load(Ordering::SeqCst) {
                    stopped(&app, &run_id);
                    return;
                }
                continue;
            }

            let result = tools::execute(&config.workspace, &call.name, &args).await;
            let (ok, output) = match result {
                Ok(output) => (true, cap(output, MAX_TOOL_RESULT_CHARS)),
                Err(error) => (false, error),
            };
            emit(
                &app,
                "agent:tool_result",
                ToolResultEvent {
                    run_id: run_id.clone(),
                    id: call.id.clone(),
                    ok,
                    output: output.clone(),
                },
            );
            messages.push(tool_message(&call.id, output));
        }
    }

    fail(
        &app,
        &run_id,
        format!("Stopped after {MAX_STEPS} steps without a final answer."),
    );
}

async fn stream_step(
    app: &AppHandle,
    run_id: &str,
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    cancel: &Arc<AtomicBool>,
) -> Result<(String, Vec<ToolCall>), String> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let body = json!({
        "model": config.model,
        "messages": messages,
        "tools": tools::schemas(),
        "stream": true,
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
    let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();

    let mut cancel_tick = tokio::time::interval(Duration::from_millis(200));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = cancel_tick.tick() => {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Stopped.".into());
                }
            }
            item = stream.next() => {
                let Some(chunk) = item else { break };
                let chunk = chunk.map_err(|err| format!("Stream interrupted: {err}"))?;
                buffer.extend_from_slice(&chunk);

                while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    if let Some(text) = apply_sse_line(&line, &mut content, &mut calls)? {
                        emit(
                            app,
                            "agent:delta",
                            DeltaEvent {
                                run_id: run_id.to_string(),
                                text,
                            },
                        );
                    }
                }
            }
        }
    }

    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer);
        if let Some(text) = apply_sse_line(&line, &mut content, &mut calls)? {
            emit(
                app,
                "agent:delta",
                DeltaEvent {
                    run_id: run_id.to_string(),
                    text,
                },
            );
        }
    }

    let mut calls: Vec<ToolCall> = calls.into_values().collect();
    for (index, call) in calls.iter_mut().enumerate() {
        if call.id.is_empty() {
            call.id = format!("call_{index}");
        }
    }
    Ok((content, calls))
}

async fn request_approval(
    app: &AppHandle,
    state: &SharedState,
    run_id: &str,
    id: &str,
    args: &Value,
    cancel: &Arc<AtomicBool>,
) -> bool {
    if cancel.load(Ordering::SeqCst) {
        return false;
    }

    let request_id = state.next_id();
    let (sender, receiver) = oneshot::channel();
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.insert(
            request_id.clone(),
            PendingApproval {
                run_id: run_id.to_string(),
                sender,
            },
        );
    } else {
        return false;
    }
    if cancel.load(Ordering::SeqCst) {
        if let Ok(mut approvals) = state.approvals.lock() {
            approvals.remove(&request_id);
        }
        return false;
    }

    emit(
        app,
        "agent:approval",
        ApprovalEvent {
            run_id: run_id.to_string(),
            id: id.to_string(),
            request_id: request_id.clone(),
            tool: "run_command".into(),
            args: args.clone(),
        },
    );

    let decision = tokio::time::timeout(APPROVAL_TIMEOUT, receiver).await;
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.remove(&request_id);
    }

    match decision {
        Ok(Ok(allow)) => allow && !cancel.load(Ordering::SeqCst),
        _ => false,
    }
}

fn apply_sse_line(
    line: &str,
    content: &mut String,
    calls: &mut BTreeMap<usize, ToolCall>,
) -> Result<Option<String>, String> {
    let Some(data) = line.trim_end().strip_prefix("data:") else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
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

    let Some(delta) = value.pointer("/choices/0/delta") else {
        return Ok(None);
    };

    let mut emitted = None;
    if let Some(text) = delta.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push_str(text);
        emitted = Some(text.to_string());
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

fn initial_messages(config: &AppConfig, input: Vec<Value>) -> Vec<Value> {
    let mut messages = Vec::with_capacity(input.len() + 1);
    messages.push(json!({ "role": "system", "content": system_prompt(config) }));
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

fn assistant_message(content: &str, calls: &[ToolCall]) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });
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

fn cap(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut capped: String = text.chars().take(max_chars).collect();
    capped.push_str("\n... [truncated]");
    capped
}

fn system_prompt(config: &AppConfig) -> String {
    format!(
        "You are Oaklet, a coding agent running locally in a desktop app. You help the user \
inspect and change the project in their workspace.\n\n\
Workspace: {workspace}\nPlatform: {platform}\n\n\
Rules:\n\
- Use the tools to inspect and edit files. Never invent file contents or claim work you did not do.\n\
- Paths are relative to the workspace; absolute paths inside it also work.\n\
- Read a file before editing it. Prefer edit_file for small changes; write_file replaces the whole file, so include its complete contents.\n\
- run_command executes in the workspace. The user must approve every command, so keep commands focused.\n\
- Keep the final answer short: what changed and how to verify it.",
        workspace = config.workspace,
        platform = std::env::consts::OS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_long_output_at_char_boundaries() {
        let text = "日本語".repeat(10);
        let capped = cap(text, 5);
        assert!(capped.starts_with("日本語日本"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn parses_empty_and_invalid_arguments() {
        assert_eq!(parse_args("").unwrap(), json!({}));
        assert!(parse_args("not json").is_err());
        assert!(parse_args("[1,2]").is_err());
    }

    #[test]
    fn accumulates_content_and_streamed_tool_calls() {
        let mut content = String::new();
        let mut calls = BTreeMap::new();

        let delta = apply_sse_line(
            r#"data: {"choices":[{"delta":{"content":"Let me look."}}]}"#,
            &mut content,
            &mut calls,
        )
        .unwrap();
        assert_eq!(delta.as_deref(), Some("Let me look."));

        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            &mut content,
            &mut calls,
        )
        .unwrap();
        apply_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"src\"}"}}]}}]}"#,
            &mut content,
            &mut calls,
        )
        .unwrap();

        assert_eq!(content, "Let me look.");
        let calls: Vec<ToolCall> = calls.into_values().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"src"}"#);
    }

    #[test]
    fn ignores_comments_done_and_surfaces_model_errors() {
        let mut content = String::new();
        let mut calls = BTreeMap::new();
        assert_eq!(
            apply_sse_line(": keep-alive", &mut content, &mut calls).unwrap(),
            None
        );
        assert_eq!(
            apply_sse_line("data: [DONE]", &mut content, &mut calls).unwrap(),
            None
        );
        assert!(
            apply_sse_line(
                r#"data: {"error":{"message":"bad key"}}"#,
                &mut content,
                &mut calls
            )
            .unwrap_err()
            .contains("bad key")
        );
    }

    #[test]
    fn rebuilds_the_system_prompt_and_drops_stale_ones() {
        let config = AppConfig::default();
        let messages = initial_messages(
            &config,
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
    fn builds_tool_messages_with_ids() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a\"}".into(),
        };
        let message = assistant_message("", &[call]);
        assert_eq!(message["content"], "");
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            tool_message("call_1", "hi".into())["tool_call_id"],
            "call_1"
        );
    }
}
