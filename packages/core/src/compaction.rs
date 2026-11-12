//! Trim tool output before replacing old conversation with a checkpoint when
//! the prompt approaches the configured context window.
//!
//! The system prompt and the newest turn stay untouched; everything between
//! them becomes one `user` message so the run can continue instead of failing
//! with a context-length error. The history is a plain array here, so a failed
//! compaction never leaves a half-written state behind.
//!
//! The summary request replays the untouched prefix and appends one directive,
//! so it is a genuine prefix of the conversation and can hit the provider's
//! warm cache. Flattening the range into a fresh prompt would re-bill every
//! token and can even exceed the size of the history it replaces.
//!
//! When the provider rejects a request for size anyway, [`force`] makes a
//! second, more aggressive pass: it trims old tool output, summarizes with a
//! smaller retained tail, and drops the oldest turn when that is still too
//! large.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::AppConfig;
use crate::context::{estimate_message, estimate_text, estimate_tokens};
use crate::repeat::is_reminder;
use crate::tools::{Cancel, ToolRegistry, cap, spill_pointer};

/// Room kept for the response and for the next round of tool output.
const OUTPUT_RESERVE: usize = 32_768;

/// Below this, a summary would cost more than the history it replaces.
const MIN_REPLACED: usize = 4_000;

/// Normal pressure keeps more detail than overflow recovery, while fitting
/// twenty typical tool results in a 32k window without a summary request.
const PRESSURE_TOOL_OUTPUT_CHARS: usize = 4_500;
const PRESSURE_TOOL_OUTPUT_HEAD_CHARS: usize = 3_000;
const PRESSURE_TOOL_OUTPUT_TAIL_CHARS: usize = 1_000;

/// Tool output kept during overflow recovery. The normal pipeline already
/// caps one result at 12k characters, but a long run can accumulate enough of
/// them to overflow anyway.
const FORCE_TOOL_OUTPUT_CHARS: usize = 2_000;
const FORCE_TOOL_OUTPUT_HEAD_CHARS: usize = 1_400;
const FORCE_TOOL_OUTPUT_TAIL_CHARS: usize = 400;
const FORCE_TOOL_OUTPUT_MARKER: &str = "\n\n[... tool output trimmed for context recovery; \
     read the archived original with history_read if the middle matters ...]\n\n";

/// Overflow recovery keeps only a small recent tail. The summary carries the
/// rest, and the current request is never cut. Normal compaction retains about
/// a sixth of the window; a provider rejection means that budget was too big.
const FORCE_RETAIN_TOKENS: usize = 2_000;

const SUMMARY_MAX_TOKENS: usize = 4_000;
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(240);
/// Bound both each text message and the whole fallback, so many short turns cannot overflow it.
const DIGEST_TEXT_CHARS: usize = 4_000;
const DIGEST_TOOL_CHARS: usize = 500;
const ARCHIVE_NOTICE: &str = "Earlier messages replaced by this checkpoint are archived; call \
history_read if you need their exact commands, errors, or file contents.";

/// A compacted history plus the original messages it replaced.
pub struct Compacted {
    pub messages: Vec<Value>,
    pub replaced: Vec<Value>,
}

/// Last resort when neither a model summary nor a mechanical digest shrinks history.
const DROPPED: &str = "The earlier part of this conversation was dropped so the run could stay \
within the context window. The original messages are archived: use history_read when you need an \
exact earlier command, error, or file content instead of assuming what happened before.";

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

/// Prompt size at which history gets compacted.
///
/// Compacting at the full window leaves no room for the response that will be
/// requested next, so keep a reserve and stop at 80% at the latest.
pub fn trigger_at(window: usize) -> usize {
    let reserve = OUTPUT_RESERVE.min(window / 4);
    window.saturating_sub(reserve).min(window * 4 / 5)
}

/// How much recent history a compaction keeps verbatim.
pub fn retain_at(window: usize) -> usize {
    (window / 6).max(2_000).min(window / 2)
}

/// Retain used when the provider has already rejected the prompt for size.
///
/// Normal compaction can leave a tail that is still too large when the real
/// window is smaller than the configured one. Keep only a small tail so the
/// retry has real room instead of a token shave.
fn force_retain_at(window: usize) -> usize {
    let retain = retain_at(window);
    (retain / 2).clamp(1_000, FORCE_RETAIN_TOKENS).min(retain)
}

/// Try tool-output trimming first, then summarize the untouched history if needed.
///
/// Returns `Ok(None)` when there is nothing worth compacting. Only a cancelled
/// run returns an error; a failed summary falls back to a mechanical digest.
pub async fn compact(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    pressure: usize,
    tools: &ToolRegistry,
    cancel: &Cancel,
) -> Result<Option<Compacted>, String> {
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let (trimmed, replaced) = trim_tool_outputs(
        messages,
        PRESSURE_TOOL_OUTPUT_CHARS,
        PRESSURE_TOOL_OUTPUT_HEAD_CHARS,
        PRESSURE_TOOL_OUTPUT_TAIL_CHARS,
    );
    let saved = estimate_tokens(messages).saturating_sub(estimate_tokens(&trimmed));
    if !replaced.is_empty() && pressure.saturating_sub(saved) <= trigger_at(config.context_window) {
        return Ok(Some(Compacted {
            messages: trimmed,
            replaced: replaced
                .into_iter()
                .map(|index| messages[index].clone())
                .collect(),
        }));
    }
    // Keep the summary request's prefix unchanged when trimming alone is insufficient.
    let Some(cut) = select_cut(messages, retain_at(config.context_window), MIN_REPLACED) else {
        return Ok(None);
    };

    Ok(Some(
        compact_at(client, config, messages, cut, Some(tools), cancel, true).await?,
    ))
}

/// Aggressively compact after a provider reports that the prompt is over its
/// context window.
///
/// Normal compaction waits for the local estimate and keeps a comfortable
/// recent tail. A provider rejection means that estimate was too optimistic, so
/// this pass uses a smaller retain and works in layers: first it trims oversized
/// tool output (L0), then it summarizes the older turns (L1). If replaying the
/// summary prefix would itself overflow, that range gets a mechanical checkpoint
/// without a model call. Finally, the oldest retained turn is dropped
/// when the result is still too large; the system prompt and the current request
/// are never dropped.
pub async fn force(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    tools: &ToolRegistry,
    cancel: &Cancel,
) -> Result<Option<Compacted>, String> {
    let target = force_retain_at(config.context_window);
    let (mut next, mut replaced) = trim_tool_outputs(
        messages,
        FORCE_TOOL_OUTPUT_CHARS,
        FORCE_TOOL_OUTPUT_HEAD_CHARS,
        FORCE_TOOL_OUTPUT_TAIL_CHARS,
    );
    let mut origins: Vec<Option<usize>> = (0..messages.len()).map(Some).collect();

    if estimate_tokens(&next) > target
        && let Some(cut) = select_cut(&next, target, MIN_REPLACED.min(target))
    {
        let prefix_fits = estimate_tokens(&next[..cut]) + estimate_text(COMPACTION_INSTRUCTION)
            <= trigger_at(config.context_window);
        let merged = cut < next.len() && role(&next[cut]) == Some("user");
        next = if prefix_fits {
            let compacted =
                compact_at(client, config, &next, cut, Some(tools), cancel, true).await?;
            compacted.messages
        } else {
            note_fallback("summary prefix would not fit");
            assemble(&next, cut, &fallback_summary(&next, cut))
        };
        // Trim/summarize/drop rewrite or remove messages; collect their original
        // indices so the archive always holds the untrimmed text.
        replaced.extend(1..cut);
        origins = reassemble_origins(&origins, cut, merged);
    }

    while estimate_tokens(&next) > target {
        let Some((dropped, removed)) = drop_oldest_turn(&next) else {
            break;
        };
        replaced.extend(origins.drain(removed).flatten());
        next = dropped;
    }

    if next == messages {
        return Ok(None);
    }
    Ok(Some(Compacted {
        messages: next,
        replaced: replaced
            .into_iter()
            .map(|index| messages[index].clone())
            .collect(),
    }))
}

/// L0: shrink tool results in place without touching the conversational order.
fn trim_tool_outputs(
    messages: &[Value],
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
) -> (Vec<Value>, BTreeSet<usize>) {
    let mut replaced = BTreeSet::new();
    let next = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if role(message) != Some("tool") {
                return message.clone();
            }
            let Some(text) = message.get("content").and_then(Value::as_str) else {
                return message.clone();
            };
            if text.chars().count() <= max_chars {
                return message.clone();
            }
            let mut trimmed = message.clone();
            trimmed["content"] = json!(trim_head_and_tail(text, max_chars, head_chars, tail_chars));
            replaced.insert(index);
            trimmed
        })
        .collect();
    (next, replaced)
}

fn trim_head_and_tail(
    text: &str,
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
) -> String {
    let total = text.chars().count();
    let marker = match spill_pointer(text) {
        Some(path) => format!(
            "\n\n[... tool output trimmed for context recovery; full output saved to: {path}. \
             Use read_file with offset and limit to page through it.]\n\n"
        ),
        None => FORCE_TOOL_OUTPUT_MARKER.to_string(),
    };
    let budget = max_chars.saturating_sub(marker.chars().count());
    let head = head_chars.min(total).min(budget);
    let tail = tail_chars.min(total - head).min(budget - head);
    let mut trimmed = String::with_capacity(max_chars);
    trimmed.extend(text.chars().take(head));
    trimmed.push_str(&marker);
    trimmed.extend(text.chars().skip(total - tail));
    trimmed
}

/// Keep the per-message original index alongside an assembled history. The
/// checkpoint and system head have no original; a retained turn does.
fn reassemble_origins(origins: &[Option<usize>], cut: usize, merged: bool) -> Vec<Option<usize>> {
    let mut next = Vec::with_capacity(origins.len() - cut + 2);
    next.push(origins[0]);
    if merged {
        next.push(origins[cut]);
        next.extend_from_slice(&origins[cut + 1..]);
    } else {
        next.push(None);
        next.extend_from_slice(&origins[cut..]);
    }
    next
}

/// Drop the oldest complete turn while keeping the summary checkpoint and the
/// current request. Returns `None` when only the current turn remains.
fn drop_oldest_turn(messages: &[Value]) -> Option<(Vec<Value>, std::ops::Range<usize>)> {
    let latest = latest_user(messages);
    if latest < 2 {
        return None;
    }

    let mut start = 1;
    while start < latest && role(&messages[start]) != Some("user") {
        start += 1;
    }
    // A checkpoint at the head is the compacted history itself; sacrifice the
    // oldest turn after it instead of the summary.
    if start < latest && is_checkpoint(&messages[start]) {
        start += 1;
        while start < latest && role(&messages[start]) != Some("user") {
            start += 1;
        }
    }
    if start >= latest {
        return None;
    }

    let end = (start + 1..latest)
        .find(|&index| role(&messages[index]) == Some("user"))
        .unwrap_or(latest);
    if end <= start {
        return None;
    }

    let mut next = Vec::with_capacity(messages.len() - (end - start));
    next.extend_from_slice(&messages[..start]);
    next.extend_from_slice(&messages[end..]);
    Some((next, start..end))
}

fn is_checkpoint(message: &Value) -> bool {
    if role(message) != Some("user") {
        return false;
    }
    match message.get("content") {
        Some(Value::String(text)) => is_protected_text(text),
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .is_some_and(is_protected_text)
        }),
        _ => false,
    }
}

fn is_protected_text(text: &str) -> bool {
    text.contains("<compacted-summary>") || text.contains(DROPPED)
}

/// Summarize `messages[1..cut]` and replace that range with one checkpoint.
///
/// `drop_on_failure` keeps an automatic run fitting the window when the model
/// cannot produce a usable summary. Manual compaction passes `false`: it is not
/// under pressure, so it reports the failure and leaves the history intact.
async fn compact_at(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    cut: usize,
    tools: Option<&ToolRegistry>,
    cancel: &Cancel,
    drop_on_failure: bool,
) -> Result<Compacted, String> {
    let replaced = messages[1..cut].to_vec();
    let replaced_tokens = estimate_tokens(&replaced);

    let summary = match summarize(client, config, &messages[..cut], tools, cancel).await {
        Ok(summary) if estimate_text(&summary) < replaced_tokens => summary,
        Ok(summary) => {
            note_fallback(&format!(
                "summary was {} tokens for {replaced_tokens} replaced",
                estimate_text(&summary)
            ));
            if !drop_on_failure {
                return Err(format!(
                    "The summary was not smaller than the history it replaced ({replaced_tokens} tokens); nothing was compacted."
                ));
            }
            fallback_summary(messages, cut)
        }
        Err(err) => {
            if cancel.is_cancelled() {
                return Err("Stopped.".into());
            }
            note_fallback(&err);
            if !drop_on_failure {
                return Err(err);
            }
            fallback_summary(messages, cut)
        }
    };

    Ok(Compacted {
        messages: assemble(messages, cut, &summary),
        replaced,
    })
}

/// Deterministic fallback; include the checkpoint framing in the shrink check.
fn fallback_summary(messages: &[Value], cut: usize) -> String {
    let digest = mechanical_digest(&messages[1..cut]);
    if !digest.is_empty()
        && estimate_tokens(&assemble(messages, cut, &digest)) < estimate_tokens(messages)
    {
        digest
    } else {
        DROPPED.to_string()
    }
}

fn mechanical_digest(messages: &[Value]) -> String {
    let mut entries = Vec::new();
    for message in messages {
        let content = match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match role(message) {
            Some("user" | "assistant") => {
                let text = if role(message) == Some("user") {
                    without_checkpoints(&content)
                } else {
                    content
                };
                if !text.trim().is_empty() {
                    entries.push(format!(
                        "{}: {}",
                        role(message).unwrap(),
                        cap(text, DIGEST_TEXT_CHARS)
                    ));
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        let args = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        entries.push(format!(
                            "tool call: {name}({})",
                            cap(args.to_string(), DIGEST_TOOL_CHARS).replace(['\r', '\n'], " ")
                        ));
                    }
                }
            }
            Some("tool") => entries.push(format!(
                "tool result: {}",
                if content.chars().count() <= DIGEST_TOOL_CHARS {
                    content
                } else {
                    "[result omitted; use history_read for the original]".into()
                }
            )),
            _ => {}
        }
    }
    let digest = entries.join("\n\n");
    if digest.chars().count() <= DIGEST_TEXT_CHARS {
        return digest;
    }
    // Keep the goal at the head and recent progress at the tail when the fallback itself is long.
    let preview = crate::spill::preview(
        &digest,
        usize::MAX,
        0,
        DIGEST_TEXT_CHARS * 3 / 5,
        DIGEST_TEXT_CHARS / 4,
    );
    format!(
        "{}\n\n[... digest middle omitted; use history_read for the original ...]\n\n{}",
        preview.head, preview.tail
    )
}

/// A checkpoint can share a user message with the actual request; retain that request.
fn without_checkpoints(mut text: &str) -> String {
    let mut plain = String::new();
    while let Some((before, rest)) = text.split_once("<compacted-summary>") {
        let Some((_, after)) = rest.split_once("</compacted-summary>") else {
            break;
        };
        plain.push_str(before);
        text = after;
        if let Some(after_notice) = text.trim_start().strip_prefix(ARCHIVE_NOTICE) {
            text = after_notice;
        }
    }
    plain.push_str(text);
    plain.trim().to_string()
}

/// Compact a conversation the UI owns, which starts at its first user message.
///
/// The run loop keeps a rebuilt system prompt at index 0 because compaction
/// leaves that slot untouched. Give the history an empty one while compacting
/// and strip it from the result so the UI can save and resend it as-is. Unlike
/// automatic compaction, manual compaction keeps only the current turn verbatim
/// and summarizes everything older.
pub async fn compact_messages(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    cancel: &Cancel,
) -> Result<Option<Compacted>, String> {
    let mut history = Vec::with_capacity(messages.len() + 1);
    history.push(json!({ "role": "system", "content": "" }));
    history.extend_from_slice(messages);

    let Some(cut) = select_latest_turn(&history) else {
        return Ok(None);
    };
    if estimate_tokens(&history[1..cut]) < MIN_REPLACED {
        return Ok(None);
    }

    let compacted = compact_at(client, config, &history, cut, None, cancel, false).await?;
    Ok(Some(Compacted {
        messages: compacted.messages[1..].to_vec(),
        replaced: compacted.replaced,
    }))
}

/// Cut immediately before the latest user turn.
///
/// Manual compaction replaces every older turn with one checkpoint and keeps
/// the current turn verbatim. Starting the tail at a user message also keeps
/// the turn's tool-call/result block intact.
fn select_latest_turn(messages: &[Value]) -> Option<usize> {
    if messages.len() < 3 {
        return None;
    }
    let cut = latest_user(messages);
    (cut >= 2).then_some(cut)
}

/// Index of the latest real user message, preferring it over loop-guard advice.
fn latest_user(messages: &[Value]) -> usize {
    messages
        .iter()
        .rposition(|message| role(message) == Some("user") && !is_reminder(message))
        .or_else(|| {
            messages
                .iter()
                .rposition(|message| role(message) == Some("user"))
        })
        .unwrap_or(1)
}

/// Pick the earliest cut whose tail fits `retain`, without starting the tail
/// with an orphaned `tool` message and without touching the current request.
///
/// A `tool` message is only valid directly after the assistant message that
/// requested it, so a cut that would leave one at the front of the tail has to
/// move forward and keep the whole tool block.
fn select_cut(messages: &[Value], retain: usize, min_replaced: usize) -> Option<usize> {
    if messages.len() < 3 {
        return None;
    }

    // Never summarize the request the agent is working on right now.
    let current_turn = latest_user(messages);

    let mut suffix = estimate_tokens(messages);
    let mut replaced = 0usize;
    for i in 1..messages.len() {
        let message = estimate_message(&messages[i - 1]);
        suffix = suffix.saturating_sub(message);
        if i >= 2 {
            replaced += message;
        }
        if i >= 2
            && i <= current_turn
            && replaced >= min_replaced
            && suffix <= retain
            && role(&messages[i]) != Some("tool")
        {
            return Some(i);
        }
    }

    // Even the newest turn is over the retain budget. Keeping it whole is
    // still better than failing the request, as long as something older is
    // worth replacing.
    (current_turn >= 2 && estimate_tokens(&messages[1..current_turn]) >= min_replaced)
        .then_some(current_turn)
}

/// Rewrite the history as: system prompt, checkpoint, untouched tail.
///
/// When the tail starts with a user message the checkpoint is merged into it,
/// which avoids two consecutive user turns that some providers reject.
fn assemble(messages: &[Value], cut: usize, summary: &str) -> Vec<Value> {
    let block = format!(
        "<compacted-summary>\n{}\n</compacted-summary>\n\n{ARCHIVE_NOTICE}",
        summary.trim()
    );
    let mut next = Vec::with_capacity(messages.len() - cut + 2);
    next.push(messages[0].clone());

    if cut < messages.len() && role(&messages[cut]) == Some("user") {
        let mut merged = messages[cut].clone();
        prepend_text(&mut merged, &block);
        next.push(merged);
        next.extend_from_slice(&messages[cut + 1..]);
    } else {
        next.push(json!({ "role": "user", "content": block }));
        next.extend_from_slice(&messages[cut..]);
    }

    next
}

fn prepend_text(message: &mut Value, prefix: &str) {
    match &mut message["content"] {
        Value::String(text) => *text = format!("{prefix}\n\n{text}"),
        Value::Array(parts) => parts.insert(0, json!({ "type": "text", "text": prefix })),
        other => *other = json!(prefix),
    }
}

fn role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

async fn summarize(
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
        "temperature": 0.2,
        "max_tokens": SUMMARY_MAX_TOKENS,
    });
    if let Some(tools) = tools {
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
    let summary = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if summary.is_empty() {
        return Err("The model returned an empty summary.".into());
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
    let mut messages: Vec<Value> = prefix
        .iter()
        .filter(|message| {
            !(role(message) == Some("system")
                && message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.trim().is_empty()))
        })
        .cloned()
        .collect();
    messages.push(json!({ "role": "user", "content": COMPACTION_INSTRUCTION }));
    messages
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

    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Stopped.".into()),
        response = request.send() => response
            .map_err(|err| format!("Could not reach {}: {err}", config.base_url))?,
    };

    let status = response.status();
    let payload = response.text().await.unwrap_or_default();
    Ok((status, payload))
}

fn tool_params_rejected(status: reqwest::StatusCode, detail: &str) -> bool {
    matches!(status.as_u16(), 400 | 422) && detail.to_ascii_lowercase().contains("tool")
}

fn note_fallback(reason: &str) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] summary failed ({reason}); trying a mechanical checkpoint");
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::thread;

    use super::*;

    fn system() -> Value {
        json!({ "role": "system", "content": "You are Oaklet." })
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    fn assistant(text: &str) -> Value {
        json!({ "role": "assistant", "content": text })
    }

    fn tool(id: &str, text: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": text })
    }

    fn tool_history(count: usize) -> Vec<Value> {
        let mut messages = vec![system(), user("Fix the build; preserve public APIs.")];
        for index in 0..count {
            messages.push(json!({
                "role": "assistant",
                "content": "Investigating the build failure.",
                "tool_calls": [{
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"build.log\"}" }
                }]
            }));
            messages.push(tool(
                &format!("call_{index}"),
                &format!(
                    "{}UNIQUE ERROR: disk full{}",
                    "h".repeat(4_000),
                    "t".repeat(4_000)
                ),
            ));
        }
        messages.push(assistant(
            "The failure is located; the fix is still pending.",
        ));
        messages.push(user("Continue with the fix."));
        messages
    }

    const SUMMARY_BODY: &str =
        r#"{"choices":[{"message":{"role":"assistant","content":"Goal: ship compaction."}}]}"#;

    /// OpenAI-compatible endpoint that answers each summary request with the
    /// next canned response and records every request body.
    fn summary_server() -> (String, Arc<StdMutex<Vec<String>>>, thread::JoinHandle<()>) {
        summary_server_with(vec![(200, SUMMARY_BODY)])
    }

    fn summary_server_with(
        responses: Vec<(u16, &str)>,
    ) -> (String, Arc<StdMutex<Vec<String>>>, thread::JoinHandle<()>) {
        let responses: Vec<_> = responses
            .into_iter()
            .map(|(status, body)| (status, body.to_string()))
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let seen_by_server = seen.clone();
        let server = thread::spawn(move || {
            for (status, response_body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buffer[..read]);
                    if let Some(head) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&raw[..head]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= head + 4 + length {
                            let request =
                                String::from_utf8_lossy(&raw[head + 4..head + 4 + length]);
                            seen_by_server.lock().unwrap().push(request.to_string());
                            break;
                        }
                    }
                }
                let reason = if (200..300).contains(&status) {
                    "OK"
                } else {
                    "Bad Request"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), seen, server)
    }

    #[test]
    fn leaves_room_for_the_response_before_compacting() {
        assert_eq!(trigger_at(262_144), 209_715);
        assert_eq!(trigger_at(32_768), 24_576);
        assert_eq!(trigger_at(4_096), 3_072);
    }

    #[test]
    fn keeps_recent_history_proportional_to_the_window() {
        assert_eq!(retain_at(262_144), 43_690);
        assert_eq!(retain_at(4_096), 2_000);
    }

    #[test]
    fn forced_compaction_keeps_a_smaller_tail_than_normal_compaction() {
        assert!(force_retain_at(262_144) < retain_at(262_144));
        assert!(force_retain_at(8_192) < retain_at(8_192));
    }

    #[tokio::test]
    async fn pressure_trimming_fits_twenty_results_without_a_summary() {
        use crate::tools::CancelHandle;

        let messages = tool_history(20);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let config = AppConfig {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            context_window: 32_768,
            ..AppConfig::default()
        };
        assert!(estimate_tokens(&messages) > trigger_at(config.context_window));
        let (_handle, cancel) = CancelHandle::new();
        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let compacted = compact(
            &client,
            &config,
            &messages,
            estimate_tokens(&messages),
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(estimate_tokens(&compacted.messages) <= trigger_at(config.context_window));
        assert_eq!(compacted.messages.len(), messages.len());
        assert_eq!(
            compacted.replaced,
            messages
                .iter()
                .filter(|message| role(message) == Some("tool"))
                .cloned()
                .collect::<Vec<_>>()
        );
        for (original, next) in messages.iter().zip(&compacted.messages) {
            if role(original) == Some("tool") {
                assert_eq!(original["tool_call_id"], next["tool_call_id"]);
                let text = next["content"].as_str().unwrap();
                assert!(text.chars().count() <= PRESSURE_TOOL_OUTPUT_CHARS);
                assert!(text.chars().count() > FORCE_TOOL_OUTPUT_CHARS);
            } else {
                assert_eq!(original, next);
            }
        }
    }

    #[tokio::test]
    async fn pressure_trimming_observes_cancellation() {
        use crate::tools::CancelHandle;

        let messages = tool_history(20);
        let (handle, cancel) = CancelHandle::new();
        handle.cancel();
        let result = compact(
            &reqwest::Client::new(),
            &AppConfig::default(),
            &messages,
            estimate_tokens(&messages),
            &ToolRegistry::new(),
            &cancel,
        )
        .await;
        assert!(matches!(result, Err(error) if error == "Stopped."));
    }

    #[tokio::test]
    async fn insufficient_pressure_trimming_summarizes_the_original_prefix() {
        use crate::tools::CancelHandle;

        // The second case fits locally, but provider-reported overhead still requires a summary.
        for (count, overhead) in [(40, 0), (20, 8_000)] {
            let (base_url, seen, server) = summary_server();
            let messages = tool_history(count);
            let config = AppConfig {
                base_url,
                context_window: 32_768,
                ..AppConfig::default()
            };
            let cut =
                select_cut(&messages, retain_at(config.context_window), MIN_REPLACED).unwrap();
            let tools = ToolRegistry::new();
            let (_handle, cancel) = CancelHandle::new();
            let compacted = compact(
                &reqwest::Client::new(),
                &config,
                &messages,
                estimate_tokens(&messages) + overhead,
                &tools,
                &cancel,
            )
            .await
            .unwrap()
            .unwrap();

            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            let body: Value = serde_json::from_str(&seen[0]).unwrap();
            let replayed = body["messages"].as_array().unwrap();
            assert_eq!(&replayed[..cut], &messages[..cut]);
            assert_eq!(replayed.len(), cut + 1);
            assert_eq!(body["tools"], tools.schemas());
            assert_eq!(compacted.replaced, messages[1..cut]);
            drop(seen);
            server.join().unwrap();
        }
    }

    #[test]
    fn pressure_then_force_trimming_keeps_the_spill_pointer() {
        let path = ".oaklet/spill/run-1/read_file-call_1.txt";
        let text = format!(
            "{}\n\n[... tool output middle pruned; full output (900 lines, 20000 bytes) saved to: {path}. Use read_file with offset and limit to page through it.]\n\n{}",
            "h".repeat(4_000),
            "t".repeat(1_500)
        );
        let pressure = trim_head_and_tail(
            &text,
            PRESSURE_TOOL_OUTPUT_CHARS,
            PRESSURE_TOOL_OUTPUT_HEAD_CHARS,
            PRESSURE_TOOL_OUTPUT_TAIL_CHARS,
        );
        let forced = trim_head_and_tail(
            &pressure,
            FORCE_TOOL_OUTPUT_CHARS,
            FORCE_TOOL_OUTPUT_HEAD_CHARS,
            FORCE_TOOL_OUTPUT_TAIL_CHARS,
        );

        assert_eq!(spill_pointer(&forced), Some(path));
        assert!(forced.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
    }

    #[tokio::test]
    async fn automatic_summary_failures_keep_a_recallable_mechanical_checkpoint() {
        use crate::store::{ContextUsage, Store};
        use crate::tools::history::HistoryRead;
        use crate::tools::test_support::context;
        use crate::tools::{CancelHandle, Tool};

        let oversized =
            json!({"choices": [{"message": {"content": "z".repeat(100_000)}}]}).to_string();
        for (status, payload) in [
            (200, r#"{"choices":[{"message":{"content":""}}]}"#),
            (200, oversized.as_str()),
            (500, r#"{"error":{"message":"unavailable"}}"#),
        ] {
            let (base_url, _seen, server) = summary_server_with(vec![(status, payload)]);
            let config = AppConfig {
                base_url,
                context_window: 32_768,
                ..AppConfig::default()
            };
            let mut messages = tool_history(3);
            messages[7]["content"] = json!("error: disk full");
            let cut = messages.len() - 1;
            let (_handle, cancel) = CancelHandle::new();
            let compacted = compact_at(
                &reqwest::Client::new(),
                &config,
                &messages,
                cut,
                Some(&ToolRegistry::new()),
                &cancel,
                true,
            )
            .await
            .unwrap();
            server.join().unwrap();

            let checkpoint = compacted.messages[1]["content"].as_str().unwrap();
            assert!(checkpoint.contains("Fix the build; preserve public APIs."));
            assert!(checkpoint.contains("The failure is located; the fix is still pending."));
            assert!(checkpoint.contains("read_file({\"path\":\"build.log\"})"));
            assert!(checkpoint.contains("error: disk full"));
            assert!(!checkpoint.contains("UNIQUE ERROR"));
            assert!(!checkpoint.contains(DROPPED));
            assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
            assert_eq!(compacted.replaced, messages[1..cut]);

            let store = Store::in_memory().unwrap();
            store.create_session("s1", "/tmp", 1).unwrap();
            store
                .apply_compaction(
                    "s1",
                    &compacted.messages[1..],
                    &compacted.replaced,
                    &json!({"kind": "notice"}),
                    ContextUsage {
                        used: estimate_tokens(&compacted.messages),
                        window: config.context_window,
                    },
                    2,
                )
                .unwrap();
            let history = HistoryRead::new(store, "s1".into(), Arc::new(StdMutex::new(Vec::new())));
            let (ctx, root, _handle) = context("digest-recall");
            let original = history
                .execute(&ctx, json!({"offset": 3, "limit": 1}))
                .await
                .unwrap();
            assert!(original.contains("UNIQUE ERROR: disk full"));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mechanical_digest_skips_old_checkpoints_but_keeps_merged_requests() {
        let merged = assemble(
            &[
                system(),
                user("old"),
                assistant("done"),
                json!({"role": "user", "content": [{"type": "text", "text": "REAL USER REQUIREMENT"}]}),
            ],
            3,
            "OBSOLETE SUMMARY",
        );
        let mut messages = merged[1..].to_vec();
        messages.push(json!({
            "role": "assistant", "content": format!("{}TEXT BEYOND CAP", "a".repeat(4_000)),
            "tool_calls": [{"function": {"name": "read_file", "arguments": format!("{}\nARGS BEYOND CAP", "x".repeat(500))}}]
        }));
        let digest = mechanical_digest(&messages);

        assert!(digest.contains("REAL USER REQUIREMENT"));
        assert!(!digest.contains("OBSOLETE SUMMARY"));
        assert!(!digest.contains(ARCHIVE_NOTICE));
        assert!(!digest.contains("TEXT BEYOND CAP"));
        assert!(!digest.contains("ARGS BEYOND CAP"));
        assert!(digest.chars().count() <= DIGEST_TEXT_CHARS);
        assert!(
            digest
                .lines()
                .any(|line| line.starts_with("tool call: read_file(") && line.ends_with(')'))
        );
        assert_eq!(
            mechanical_digest(&[user("<compacted-summary>old</compacted-summary>")]),
            ""
        );
    }

    #[test]
    fn mechanical_fallback_drops_only_when_the_checkpoint_would_not_shrink() {
        let messages = vec![system(), user("hi"), assistant("hello"), user("continue")];
        assert_eq!(fallback_summary(&messages, 3), DROPPED);
    }

    #[tokio::test]
    async fn force_uses_a_digest_when_the_summary_prefix_does_not_fit() {
        use crate::tools::CancelHandle;

        let messages = tool_history(20);
        let config = AppConfig {
            base_url: "invalid://no-summary".into(),
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let compacted = force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .unwrap();
        let checkpoint = compacted.messages[1]["content"].as_str().unwrap();

        assert!(checkpoint.contains("Fix the build; preserve public APIs."));
        assert!(checkpoint.contains("read_file("));
        assert!(!checkpoint.contains(DROPPED));
        assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
        assert!(
            compacted
                .replaced
                .iter()
                .any(|message| message == &messages[3])
        );
    }

    #[test]
    fn trims_oversized_tool_output_for_recovery() {
        let original = "x".repeat(20_000);
        let messages = vec![system(), user("go"), tool("call_1", &original)];

        let (trimmed, replaced) = trim_tool_outputs(
            &messages,
            FORCE_TOOL_OUTPUT_CHARS,
            FORCE_TOOL_OUTPUT_HEAD_CHARS,
            FORCE_TOOL_OUTPUT_TAIL_CHARS,
        );
        let text = trimmed[2]["content"].as_str().unwrap();

        assert!(text.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
        assert!(text.contains(FORCE_TOOL_OUTPUT_MARKER));
        assert!(text.contains("history_read"));
        assert!(text.starts_with('x') && text.ends_with('x'));
        assert_eq!(replaced.into_iter().collect::<Vec<_>>(), vec![2]);
        assert_eq!(messages[2]["content"].as_str().unwrap(), original);
    }

    #[test]
    fn keeps_an_existing_spill_pointer_when_trimming_again() {
        let path = ".oaklet/spill/run-1/run_command-call_1.txt";
        let original = format!(
            "artifact saved to: build/report.html\n{}\n\n[... tool output middle pruned; full output (900 lines, 20000 bytes) saved to: {path}. Use read_file with offset and limit to page through it.]\n\n{}",
            "h".repeat(4_000),
            "t".repeat(4_000)
        );

        let trimmed = trim_head_and_tail(
            &original,
            FORCE_TOOL_OUTPUT_CHARS,
            FORCE_TOOL_OUTPUT_HEAD_CHARS,
            FORCE_TOOL_OUTPUT_TAIL_CHARS,
        );

        assert!(trimmed.contains(path));
        assert!(trimmed.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
    }

    #[test]
    fn drops_the_oldest_turn_but_keeps_the_checkpoint_and_current_request() {
        let messages = vec![
            system(),
            user("<compacted-summary>\nold work\n</compacted-summary>"),
            user("old request"),
            assistant("old answer"),
            user("current request"),
            assistant("current answer"),
        ];

        let (next, removed) = drop_oldest_turn(&messages).unwrap();
        let removed = &messages[removed];

        assert_eq!(next.len(), 4);
        assert_eq!(removed, &[user("old request"), assistant("old answer")]);
        assert!(
            next[1]["content"]
                .as_str()
                .unwrap()
                .contains("<compacted-summary>")
        );
        assert!(
            !next
                .iter()
                .any(|message| message["content"] == "old request")
        );
        assert!(
            next.iter()
                .any(|message| message["content"] == "current request")
        );
    }

    #[test]
    fn keeps_the_dropped_notice_while_shedding_old_turns() {
        let messages = vec![
            system(),
            user(DROPPED),
            user("old request"),
            assistant("old answer"),
            user("current request"),
        ];

        let (next, removed) = drop_oldest_turn(&messages).unwrap();

        assert_eq!(next.len(), 3);
        assert_eq!(removed.len(), 2);
        assert!(next[1]["content"].as_str().unwrap().contains(DROPPED));
        assert!(
            !next
                .iter()
                .any(|message| message["content"] == "old request")
        );
        assert_eq!(next.last().unwrap()["content"], "current request");
    }

    #[test]
    fn never_starts_the_tail_with_an_orphan_tool_message() {
        let mut messages = vec![system()];
        for step in 0..12 {
            messages.push(user(&format!("request {step}")));
            messages.push(json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": format!("call_{step}"),
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{}" }
                }]
            }));
            messages.push(tool(&format!("call_{step}"), &"x".repeat(800)));
        }

        let cut = select_cut(&messages, 500, 100).unwrap();
        assert_ne!(role(&messages[cut]), Some("tool"));
        assert!(estimate_tokens(&messages[cut..]) <= 500);
        assert!(cut >= 2);
    }

    #[test]
    fn skips_a_conversation_with_nothing_old_enough_to_replace() {
        let messages = vec![system(), user("hi"), assistant("hello")];
        assert_eq!(select_cut(&messages, 100_000, MIN_REPLACED), None);
    }

    #[test]
    fn never_summarizes_the_latest_user_request() {
        let mut messages = vec![system()];
        for step in 0..20 {
            messages.push(user(&format!("old {step} {}", "x".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        messages.push(user("CURRENT REQUEST"));
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_now",
                "type": "function",
                "function": { "name": "read_file", "arguments": "{}" }
            }]
        }));
        messages.push(tool("call_now", &"r".repeat(400)));

        let cut = select_cut(&messages, 500, 100).unwrap();
        assert!(cut <= messages.len() - 3);
        assert!(
            messages[cut..]
                .iter()
                .any(|message| message["content"] == "CURRENT REQUEST")
        );
    }

    #[test]
    fn a_loop_guard_reminder_does_not_replace_the_current_request() {
        let mut messages = vec![system()];
        for step in 0..20 {
            messages.push(user(&format!("old {step} {}", "x".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        messages.push(user("CURRENT REQUEST"));
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_now",
                "type": "function",
                "function": { "name": "read_file", "arguments": "{}" }
            }]
        }));
        messages.push(tool("call_now", &"r".repeat(400)));
        messages.push(user(&format!(
            "{}you have called `read_file` 3 times in a row",
            crate::repeat::REMINDER_PREFIX
        )));
        messages.push(assistant("trying another approach"));

        let cut = select_cut(&messages, 500, 100).unwrap();
        assert!(
            messages[cut..]
                .iter()
                .any(|message| message["content"] == "CURRENT REQUEST")
        );
    }

    #[test]
    fn falls_back_to_the_current_turn_when_it_exceeds_the_retain_budget() {
        let mut messages = vec![system()];
        for _ in 0..5 {
            messages.push(user(&"old ".repeat(100)));
            messages.push(assistant(&"a".repeat(800)));
        }
        messages.push(user(&"x".repeat(20_000)));

        let cut = select_cut(&messages, 100, 100).unwrap();
        assert_eq!(cut, messages.len() - 1);
        assert_eq!(role(&messages[cut]), Some("user"));
    }

    #[test]
    fn merges_the_checkpoint_into_a_leading_user_turn() {
        let messages = vec![system(), user("old"), assistant("working"), user("new")];
        let next = assemble(&messages, 3, "checkpoint");
        assert_eq!(next.len(), 2);
        assert_eq!(next[0]["role"], "system");
        let content = next[1]["content"].as_str().unwrap();
        assert!(content.contains("checkpoint"));
        assert!(content.contains("new"));
    }

    #[test]
    fn inserts_the_checkpoint_before_an_assistant_tail() {
        let messages = vec![system(), user("old"), assistant("kept")];
        let next = assemble(&messages, 2, "checkpoint");
        assert_eq!(next.len(), 3);
        assert_eq!(next[1]["role"], "user");
        assert!(next[1]["content"].as_str().unwrap().contains("checkpoint"));
        assert_eq!(next[2]["content"], "kept");
    }

    #[test]
    fn summary_messages_replay_tool_history_before_the_directive() {
        let messages = vec![
            json!({ "role": "system", "content": "" }),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" }
                }]
            }),
            tool("call_1", "file body"),
        ];
        let replayed = summary_messages(&messages);
        assert_eq!(replayed.len(), 3);
        assert_eq!(
            replayed[0]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(replayed[1]["content"], "file body");
        assert_eq!(replayed[2]["role"], "user");
        assert!(
            replayed[2]["content"]
                .as_str()
                .unwrap()
                .contains("checkpoint")
        );
    }

    #[test]
    fn summary_uses_the_main_model_unless_overridden() {
        let mut config = AppConfig::default();
        assert_eq!(summary_model(&config), config.model.as_str());
        config.summary_model = "cheap-model".into();
        assert_eq!(summary_model(&config), "cheap-model");
    }

    #[tokio::test]
    async fn replaces_the_middle_with_the_model_summary() {
        use crate::tools::CancelHandle;

        let (base_url, seen, server) = summary_server();

        let mut messages = vec![system()];
        for step in 0..30 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let last = messages.last().unwrap().clone();

        let config = AppConfig {
            base_url,
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let client = reqwest::Client::new();
        let tools = ToolRegistry::new();

        let compacted = compact(
            &client,
            &config,
            &messages,
            estimate_tokens(&messages),
            &tools,
            &cancel,
        )
        .await
        .unwrap()
        .expect("compaction should apply");
        let next = compacted.messages;

        server.join().unwrap();
        assert!(compacted.replaced.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("request 0"))
        }));
        assert!(
            compacted
                .replaced
                .iter()
                .all(|message| message["role"] != "system")
        );
        assert!(next.len() < messages.len());
        assert_eq!(next[0]["role"], "system");
        assert!(next.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("ship compaction."))
        }));
        assert_eq!(next.last().unwrap(), &last);

        let seen = seen.lock().unwrap();
        let body: Value = serde_json::from_str(&seen[0]).unwrap();
        assert_eq!(body["model"], config.model);
        assert_eq!(body["tool_choice"], "none");
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "read_file")
        );
        let replayed = body["messages"].as_array().unwrap();
        assert_eq!(replayed[0]["content"], "You are Oaklet.");
        assert_eq!(replayed.last().unwrap()["role"], "user");
        assert!(
            replayed.last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("at most 800 words")
        );
        assert!(replayed.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("request 0"))
        }));
    }

    #[tokio::test]
    async fn force_summarizes_with_a_smaller_tail() {
        use crate::tools::CancelHandle;

        let (base_url, seen, server) = summary_server();
        let mut messages = vec![system()];
        for step in 0..30 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let last = messages.last().unwrap().clone();

        let config = AppConfig {
            base_url,
            context_window: 32_768,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let compacted = force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .expect("forced compaction should apply");
        let next = compacted.messages;

        server.join().unwrap();
        assert!(!compacted.replaced.is_empty());
        assert!(compacted.replaced.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("request 0 "))
        }));
        assert!(
            compacted
                .replaced
                .iter()
                .all(|message| message["role"] != "system")
        );
        assert!(estimate_tokens(&next) < estimate_tokens(&messages));
        assert_eq!(next.last().unwrap(), &last);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let body: Value = serde_json::from_str(&seen[0]).unwrap();
        assert_eq!(body["tool_choice"], "none");
        assert!(body.get("tools").is_some());
    }

    #[tokio::test]
    async fn force_archives_the_untrimmed_tool_output_before_summarizing() {
        use crate::tools::CancelHandle;

        let (base_url, _seen, server) = summary_server();
        let original = format!(
            "{}\nUNIQUE ERROR: disk full\n{}",
            "h".repeat(4_000),
            "t".repeat(4_000)
        );
        let mut messages = vec![
            system(),
            user("old request"),
            assistant("reading"),
            tool("call_1", &original),
        ];
        for step in 0..30 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        messages.push(user("current request"));

        let config = AppConfig {
            base_url,
            context_window: 32_768,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let compacted = force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .expect("forced compaction should apply");

        server.join().unwrap();
        assert!(
            compacted
                .replaced
                .iter()
                .any(|message| { message["content"].as_str() == Some(original.as_str()) })
        );
    }

    #[tokio::test]
    async fn force_skips_the_summary_when_trimming_tool_output_is_enough() {
        use crate::tools::CancelHandle;

        let original = format!(
            "HEAD\n{}\nUNIQUE ERROR: disk full\n{}",
            "x".repeat(4_000),
            "y".repeat(4_000)
        );
        let messages = vec![
            system(),
            user("old request"),
            assistant("reading"),
            tool("call_1", &original),
            user("current request"),
        ];
        let config = AppConfig {
            base_url: "http://127.0.0.1:1".into(),
            context_window: 32_768,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();

        let compacted = force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .expect("trimmable tool output should count as progress");
        let next = compacted.messages;

        assert_eq!(compacted.replaced.len(), 1);
        assert_eq!(
            compacted.replaced[0]["content"], original,
            "the untrimmed tool result is archived for history_read"
        );
        assert!(
            compacted.replaced[0]["content"]
                .as_str()
                .unwrap()
                .contains("UNIQUE ERROR: disk full")
        );
        assert!(
            next.iter()
                .any(|message| message["content"] == "old request")
        );
        assert_eq!(next.last().unwrap()["content"], "current request");
        assert!(next.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains(FORCE_TOOL_OUTPUT_MARKER))
        }));
    }

    #[tokio::test]
    async fn force_keeps_a_bounded_digest_when_the_summary_prefix_would_overflow() {
        use crate::tools::CancelHandle;

        let mut messages = vec![system()];
        for step in 0..40 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let last = messages.last().unwrap().clone();

        // The prefix is far beyond this window, so force must not spend a
        // summary request that is guaranteed to be rejected.
        let config = AppConfig {
            base_url: "http://127.0.0.1:1".into(),
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let compacted = force(
            &reqwest::Client::new(),
            &config,
            &messages,
            &ToolRegistry::new(),
            &cancel,
        )
        .await
        .unwrap()
        .expect("forced compaction should apply");
        let next = compacted.messages;

        assert!(!compacted.replaced.is_empty());
        assert!(estimate_tokens(&next) < estimate_tokens(&messages));
        assert_eq!(next.last().unwrap(), &last);
        assert!(next.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("user: request 0"))
        }));
        assert!(estimate_tokens(&next) <= trigger_at(config.context_window));
    }

    #[tokio::test]
    async fn retries_without_tools_when_a_provider_rejects_tool_choice() {
        use crate::tools::CancelHandle;

        let (base_url, seen, server) = summary_server_with(vec![
            (
                400,
                r#"{"error":{"message":"unsupported parameter: tool_choice"}}"#,
            ),
            (200, SUMMARY_BODY),
        ]);
        let mut messages = vec![system()];
        for step in 0..30 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let config = AppConfig {
            base_url,
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();
        let tools = ToolRegistry::new();

        compact(
            &reqwest::Client::new(),
            &config,
            &messages,
            estimate_tokens(&messages),
            &tools,
            &cancel,
        )
        .await
        .unwrap()
        .expect("compaction should apply");

        server.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let first: Value = serde_json::from_str(&seen[0]).unwrap();
        let second: Value = serde_json::from_str(&seen[1]).unwrap();
        assert_eq!(first["tool_choice"], "none");
        assert!(first.get("tools").is_some());
        assert!(second.get("tools").is_none());
        assert!(second.get("tool_choice").is_none());
        assert_eq!(second["messages"], first["messages"]);
    }

    #[tokio::test]
    async fn compacts_ui_history_without_a_system_prompt() {
        use crate::tools::CancelHandle;

        let (base_url, _seen, server) = summary_server();
        let mut messages = Vec::new();
        for step in 0..30 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let last = messages.last().unwrap().clone();

        let config = AppConfig {
            base_url,
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();

        let compacted = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
            .await
            .unwrap()
            .expect("compaction should apply");
        let next = compacted.messages;

        server.join().unwrap();
        assert!(!compacted.replaced.is_empty());
        assert!(
            compacted
                .replaced
                .iter()
                .all(|message| message["role"] != "system")
        );
        assert!(compacted.replaced.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("request 0 "))
        }));
        assert_eq!(next.len(), 2);
        assert!(next.iter().all(|message| message["role"] != "system"));
        let current = next[0]["content"].as_str().unwrap();
        assert!(current.contains("ship compaction."));
        assert!(current.contains("request 29"));
        assert!(!current.contains("request 0"));
        assert_eq!(next.last().unwrap(), &last);
    }

    #[tokio::test]
    async fn manual_compaction_keeps_the_whole_latest_turn() {
        use crate::tools::CancelHandle;

        let (base_url, _seen, server) = summary_server();
        let mut messages = Vec::new();
        for step in 0..10 {
            messages.push(user(&format!("old {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        messages.push(user("LATEST REQUEST"));
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_latest",
                "type": "function",
                "function": { "name": "read_file", "arguments": "{}" }
            }]
        }));
        messages.push(tool("call_latest", &"r".repeat(800)));
        messages.push(assistant("latest answer"));

        let config = AppConfig {
            base_url,
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();

        let compacted = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
            .await
            .unwrap()
            .expect("compaction should apply");
        let next = compacted.messages;

        server.join().unwrap();
        assert!(
            !compacted
                .replaced
                .iter()
                .any(|message| message["content"] == "LATEST REQUEST")
        );
        assert_eq!(next.len(), 4);
        assert!(
            next[0]["content"]
                .as_str()
                .unwrap()
                .contains("LATEST REQUEST")
        );
        assert!(
            next.iter()
                .any(|message| message["tool_call_id"] == "call_latest")
        );
        assert!(
            next.iter()
                .any(|message| message["content"] == "latest answer")
        );
        assert!(!next.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("old 0"))
        }));
    }

    #[tokio::test]
    async fn manual_compaction_skips_a_single_turn() {
        use crate::tools::CancelHandle;

        let messages = vec![user("only request"), assistant("only answer")];
        let config = AppConfig {
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();

        let next = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
            .await
            .unwrap();

        assert!(next.is_none());
    }

    #[tokio::test]
    async fn manual_compaction_keeps_history_when_the_summary_fails() {
        use crate::tools::CancelHandle;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let mut messages = Vec::new();
        for step in 0..11 {
            messages.push(user(&format!("request {step} {}", "u".repeat(800))));
            messages.push(assistant(&"a".repeat(800)));
        }
        let config = AppConfig {
            base_url: format!("http://{address}"),
            context_window: 8_192,
            ..AppConfig::default()
        };
        let (_handle, cancel) = CancelHandle::new();

        let result = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel).await;

        assert!(result.is_err());
    }
}
