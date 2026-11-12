mod approval;
mod messages;
mod model;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::AppConfig;
use crate::chat::RunContext;
use crate::compaction;
use crate::context::{Meter, estimate_tokens};
use crate::repeat::RepeatGuard;
use crate::tools::{Cancel, ToolContext, ToolRegistry, Workspace};

use approval::TauriApprover;
use messages::{assistant_message, initial_messages, strip_system, tool_message};
use model::stream_step;

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
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

    if let Some(compacted) =
        compaction::compact(client, config, messages, pressure, tools, cancel).await?
    {
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
            // The summary request fails, so preflight must use a bounded digest.
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
                .is_some_and(|text| text.contains("user: request 0"))
        }));
    }

    #[test]
    fn parses_empty_and_invalid_arguments() {
        assert_eq!(parse_args("").unwrap(), json!({}));
        assert!(parse_args("not json").is_err());
        assert!(parse_args("[1,2]").is_err());
    }
}
