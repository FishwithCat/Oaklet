//! One agent turn: stream a reply, run its tools, repeat until the model stops
//! asking for them, then hand the transcript back through `ctx`.

mod approval;
mod messages;
mod model;
mod step;
mod tool_calls;
mod window;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::AppConfig;
use crate::chat::RunContext;
use crate::context::Meter;
use crate::instructions::WorkspaceInstructions;
use crate::repeat::RepeatGuard;
use crate::tools::{Cancel, ToolContext, ToolRegistry, Workspace};

use messages::{assistant_message, image_message, initial_messages, strip_system, system_prompt};

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
    let mut instructions = WorkspaceInstructions::load(workspace.root());
    let mut tools = ToolRegistry::with_host_tools(ctx.app(), &config).await;
    ensure_history_tool(&mut tools, &ctx);
    let tool_context = ToolContext {
        workspace,
        cancel: cancel.clone(),
        jobs: ctx.jobs(),
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

    let mut messages = initial_messages(
        &config,
        &tools,
        &model_sources,
        instructions.render().as_deref(),
        input,
    );
    let mut meter = Meter::default();
    let mut guard = RepeatGuard::new();
    // A forced compaction already shrank the prompt for the retry, so do not
    // immediately run the normal preflight on it again.
    let mut retry_directly = false;
    // Only one forced retry per step; a second rejection reports the error.
    let mut overflow_retried = false;

    loop {
        if cancel.is_cancelled() {
            ctx.finish_stopped(tail(&messages)).await;
            return;
        }

        // This also runs before the first request of a run, so a saved history
        // that no longer fits the configured window after a model switch is
        // compacted before it is sent.
        if !retry_directly
            && let Err(err) = window::make_room(
                &client,
                &config,
                &mut tools,
                &mut messages,
                &mut meter,
                &cancel,
                &ctx,
            )
            .await
        {
            finish(&ctx, &cancel, &messages, err).await;
            return;
        }
        retry_directly = false;

        // Report the size the next request will be billed for, after any
        // compaction and while the estimate is still fresh.
        window::report(&ctx, &config, &meter, &messages);

        let reply = match step::request(
            &client,
            &config,
            &mut tools,
            &mut messages,
            &cancel,
            &ctx,
            &mut overflow_retried,
        )
        .await
        {
            Ok(step::Next::Reply(reply)) => {
                if let Some(prompt_tokens) = reply.prompt_tokens {
                    meter.record_prompt_tokens(prompt_tokens);
                }
                reply
            }
            Ok(step::Next::Retry) => {
                // The retry is a different, smaller request. Price it before
                // the next usage anchor is recorded.
                let _ = meter.prepare(&messages);
                retry_directly = true;
                continue;
            }
            Err(err) => {
                finish(&ctx, &cancel, &messages, err).await;
                return;
            }
        };

        if reply.content.trim().is_empty() && reply.calls.is_empty() {
            finish(
                &ctx,
                &cancel,
                &messages,
                "The model returned an empty response.",
            )
            .await;
            return;
        }

        let assistant = assistant_message(&reply.content, &reply.reasoning, &reply.calls);
        ctx.record_message(assistant.clone(), Some(json!({ "runId": ctx.run_id() })));
        messages.push(assistant);
        ctx.context(meter.pressure(&messages), config.context_window);
        if reply.calls.is_empty() {
            // The system prompt is rebuilt every turn; keep it out of the
            // conversation that is saved.
            ctx.finish_done(tail(&messages)).await;
            return;
        }

        let batch = tool_calls::run(
            &reply.calls,
            &tools,
            &tool_context,
            &ctx,
            &cancel,
            &mut guard,
            &mut messages,
        )
        .await;
        if batch.cancelled {
            ctx.finish_stopped(tail(&messages)).await;
            return;
        }

        // Newly reached directories join the existing system prompt instead of
        // becoming a history message, so a later run starts from disk again
        // rather than replaying stale instructions.
        if reload_instructions(&mut instructions, &tool_context, &batch.instruction_touches) {
            messages[0] = json!({
                "role": "system",
                "content": system_prompt(
                    &config,
                    &tools,
                    &model_sources,
                    instructions.render().as_deref(),
                ),
            });
        }

        // A tool result cannot carry an image part on every OpenAI-compatible
        // provider. Attach the batch's images as one user turn after all tool
        // results, so the conversation stays valid and the model can see them.
        if !batch.images.is_empty() {
            let images = image_message(&batch.images);
            ctx.record_hidden(images.clone());
            messages.push(images);
        }

        // Reminders wait until every result in this batch exists, so a user
        // message never splits the tool results answering one assistant turn.
        for reminder in batch.reminders {
            let message = json!({ "role": "user", "content": reminder });
            ctx.record_hidden(message.clone());
            messages.push(message);
        }
    }
}

/// History without the system prompt, which is rebuilt for every request.
fn tail(messages: &[Value]) -> Vec<Value> {
    messages[1..].to_vec()
}

/// End the run with `error`, or as stopped when the user cancelled.
async fn finish(ctx: &RunContext, cancel: &Cancel, messages: &[Value], error: impl Into<String>) {
    let history = tail(messages);
    if cancel.is_cancelled() {
        ctx.finish_stopped(history).await;
    } else {
        ctx.finish_error(history, error).await;
    }
}

/// Load instructions from newly touched directories into the system prompt.
fn reload_instructions(
    instructions: &mut WorkspaceInstructions,
    tool_context: &ToolContext,
    touches: &[PathBuf],
) -> bool {
    let root = tool_context.workspace.root();
    let mut loaded = false;
    for path in touches {
        loaded |= instructions.touch(root, path);
    }
    loaded
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

    #[test]
    fn parses_empty_and_invalid_arguments() {
        assert_eq!(parse_args("").unwrap(), json!({}));
        assert!(parse_args("not json").is_err());
        assert!(parse_args("[1,2]").is_err());
    }
}
