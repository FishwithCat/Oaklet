//! Replay the conversation prefix for cacheable, cancellable summary requests.

use serde_json::{Value, json};
use std::time::Duration;

use super::history::role;
use crate::AppConfig;
use crate::context::{estimate_text, estimate_tokens};
use crate::tools::{Cancel, ToolRegistry, cap};

const SUMMARY_TIMEOUT: Duration = Duration::from_secs(240);

/// Base64 data URLs above this are replaced in the replay; they are priced as
/// one image but would still travel as megabytes in the request body.
const REPLAY_IMAGE_CHARS: usize = 4_000;

pub(super) fn output_limit(config: &AppConfig) -> usize {
    (config.context_window / 8).clamp(512, 4_096)
}

const COMPACTION_INSTRUCTION: &str = "\
Condense the conversation above into a checkpoint that the same coding agent will read to continue \
the task. Preserve the facts needed to keep working; drop greetings, repetition, and raw tool \
output that no longer matters.

Return Markdown only, at most 800 words, using these sections. Omit a section only when it is empty.

## Goal
What the user asked for, in their own terms, including constraints and acceptance criteria.

## Completed
Work that is finished and verified. Say how it was verified.

## Current state
What is true right now: files changed, commands run, tests passing or failing.

## Key files and paths
Exact paths, function names, symbols, and line numbers that matter.

## Errors and fixes
Exact error text, what caused it, and what fixed it (or that it is still open).

## Decisions and constraints
Choices already made that must not be revisited, plus user preferences.

## Remaining work
Concrete next actions, in order.

## Next step
The single next action the agent should take.

Rules:
- Quote exact paths, identifiers, commands, and error strings.
- Never invent work that did not happen; mark uncertain items as uncertain.
- Keep the language of the original conversation for quoted requirements.
- Do not include tool-call syntax, JSON, or thinking out loud, and do not call any tool.";

pub(super) fn prefix_tokens(messages: &[Value], cut: usize) -> usize {
    estimate_tokens(&messages[..cut]).saturating_add(crate::context::estimate_message(
        &json!({"role":"user", "content":COMPACTION_INSTRUCTION}),
    ))
}

/// Whether the summary request fits: its replayed prefix, the directive, and
/// the tool schemas it carries. A prefix that fits alone can still overflow.
pub(super) fn prefix_fits(
    messages: &[Value],
    cut: usize,
    config: &AppConfig,
    tools: Option<&ToolRegistry>,
) -> bool {
    let schemas = tools
        .map(|tools| estimate_text(&tools.schemas().to_string()))
        .unwrap_or(0);
    prefix_tokens(messages, cut).saturating_add(schemas)
        <= config.context_window.saturating_sub(output_limit(config))
}

pub(super) async fn summarize(
    client: &reqwest::Client,
    config: &AppConfig,
    prefix: &[Value],
    tools: Option<&ToolRegistry>,
    cancel: &Cancel,
) -> Result<String, String> {
    let mut body = json!({
        "model": summary_model(config),
        "messages": summary_messages(prefix),
        "stream": false,
        "max_tokens": output_limit(config),
    });
    if let Some(tools) = tools.filter(|tools| !tools.schemas().as_array().is_none_or(Vec::is_empty))
    {
        body["tools"] = tools.schemas();
        body["tool_choice"] = json!("none");
    }

    let (mut status, mut payload) = post_summary(client, config, &body, cancel).await?;
    if !status.is_success() && tools.is_some() && tool_params_rejected(status, &payload) {
        // Some OpenAI-compatible providers reject `tool_choice` on auxiliary
        // calls. Dropping `tools` still keeps the message prefix cacheable and
        // only gives up the schema segment.
        if let Some(body) = body.as_object_mut() {
            body.remove("tools");
            body.remove("tool_choice");
        }
        (status, payload) = post_summary(client, config, &body, cancel).await?;
    }

    if !status.is_success() {
        return Err(format!(
            "Summary request failed ({status}): {}",
            cap(payload, 2_000)
        ));
    }

    let value: Value = serde_json::from_str(&payload)
        .map_err(|err| format!("Malformed summary response: {err}"))?;
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        && !matches!(reason, "stop" | "end_turn" | "stop_sequence")
    {
        return Err(format!(
            "The summary did not finish ({reason}); nothing was compacted."
        ));
    }
    let summary = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if summary.is_empty() {
        return Err("The model returned an empty summary.".into());
    }
    if estimate_text(&summary) > output_limit(config) {
        return Err("The model summary exceeded its token budget.".into());
    }
    Ok(summary)
}

/// Replay the untouched prefix, then append the directive as the final user
/// message. Reusing the conversation's own system prompt and message objects
/// keeps this call a true prefix of the last request, so only the directive and
/// the summary output are uncached.
///
/// An empty system head (the placeholder manual compaction adds for indexing)
/// contributes no message.
fn summary_messages(prefix: &[Value]) -> Vec<Value> {
    let mut messages: Vec<Value> = crate::context::request_messages(prefix)
        .into_iter()
        .map(omit_oversized_images)
        .filter(|message| {
            !(role(message) == Some("system")
                && message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.trim().is_empty()))
        })
        .collect();
    messages.push(json!({ "role": "user", "content": COMPACTION_INSTRUCTION }));
    messages
}

fn omit_oversized_images(mut message: Value) -> Value {
    let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return message;
    };
    for part in parts {
        let is_image = matches!(
            part.get("type").and_then(Value::as_str),
            Some("image_url" | "input_image")
        );
        if !is_image {
            continue;
        }
        let url = part
            .pointer("/image_url/url")
            .and_then(Value::as_str)
            .or_else(|| part.get("image_url").and_then(Value::as_str))
            .or_else(|| part.get("url").and_then(Value::as_str));
        if url.is_some_and(|url| url.chars().count() > REPLAY_IMAGE_CHARS) {
            *part = json!({
                "type": "text",
                "text": "[image omitted from the summary replay to keep the request bounded]",
            });
        }
    }
    message
}

/// Empty means the main model, which lets the summary request reuse that
/// model's warm prefix cache.
fn summary_model(config: &AppConfig) -> &str {
    let configured = config.summary_model.trim();
    if configured.is_empty() {
        &config.model
    } else {
        configured
    }
}

async fn post_summary(
    client: &reqwest::Client,
    config: &AppConfig,
    body: &Value,
    cancel: &Cancel,
) -> Result<(reqwest::StatusCode, String), String> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let mut request = client.post(&url).json(body).timeout(SUMMARY_TIMEOUT);
    if !config.api_key.is_empty() {
        request = request.bearer_auth(&config.api_key);
    }

    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Stopped.".into()),
        result = async {
            let response = request.send().await
                .map_err(|err| format!("Could not reach {}: {err}", config.base_url))?;
            let status = response.status();
            let payload = response.text().await
                .map_err(|err| format!("Could not read summary response: {err}"))?;
            Ok((status, payload))
        } => result,
    }
}

fn tool_params_rejected(status: reqwest::StatusCode, detail: &str) -> bool {
    matches!(status.as_u16(), 400 | 422) && detail.to_ascii_lowercase().contains("tool")
}

#[cfg(test)]
mod tests;
