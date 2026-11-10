//! Replace old conversation with an LLM-written checkpoint when the prompt
//! approaches the configured context window.
//!
//! The system prompt and the newest turn stay untouched; everything between
//! them becomes one `user` message so the run can continue instead of failing
//! with a context-length error. The history is a plain array here, so a failed
//! compaction never leaves a half-written state behind.

use std::time::Duration;

use serde_json::{Value, json};

use crate::AppConfig;
use crate::context::{estimate_message, estimate_text, estimate_tokens};
use crate::repeat::is_reminder;
use crate::tools::{Cancel, cap};

/// Room kept for the response and for the next round of tool output.
const OUTPUT_RESERVE: usize = 32_768;

/// Below this, a summary would cost more than the history it replaces.
const MIN_REPLACED: usize = 4_000;

const SUMMARY_MAX_TOKENS: usize = 4_000;
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(240);

/// Shown instead of a model summary when summarization fails.
const DROPPED: &str = "The earlier part of this conversation was dropped so the run could stay \
within the context window. Treat it as unavailable: re-read files and re-run checks instead of \
assuming what happened before.";

const SUMMARIZER_PROMPT: &str = "\
You compress a coding agent's conversation into a checkpoint that the same agent will read to \
continue the task. Preserve the facts needed to keep working; drop greetings, repetition, and raw \
tool output that no longer matters.

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
- Do not include tool-call syntax, JSON, or thinking out loud.";

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

/// Summarize the oldest part of `messages`, or keep them when that is not
/// useful.
///
/// Returns `Ok(None)` when there is nothing worth compacting. Only a cancelled
/// run returns an error; a failed summary falls back to dropping the old
/// history so the request still fits.
pub async fn compact(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    cancel: &Cancel,
) -> Result<Option<Vec<Value>>, String> {
    let Some(cut) = select_cut(messages, retain_at(config.context_window), MIN_REPLACED) else {
        return Ok(None);
    };

    Ok(Some(
        compact_at(client, config, messages, cut, cancel, true).await?,
    ))
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
    cancel: &Cancel,
    drop_on_failure: bool,
) -> Result<Vec<Value>, String> {
    let replaced = estimate_tokens(&messages[1..cut]);
    let rendered = render(&messages[1..cut]);

    let summary = match summarize(client, config, &rendered, cancel).await {
        Ok(summary) if estimate_text(&summary) < replaced => summary,
        Ok(summary) => {
            note_fallback(&format!(
                "summary was {} tokens for {replaced} replaced",
                estimate_text(&summary)
            ));
            if !drop_on_failure {
                return Err(format!(
                    "The summary was not smaller than the history it replaced ({replaced} tokens); nothing was compacted."
                ));
            }
            DROPPED.to_string()
        }
        Err(err) => {
            if cancel.is_cancelled() {
                return Err("Stopped.".into());
            }
            note_fallback(&err);
            if !drop_on_failure {
                return Err(err);
            }
            DROPPED.to_string()
        }
    };

    Ok(assemble(messages, cut, &summary))
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
) -> Result<Option<Vec<Value>>, String> {
    let mut history = Vec::with_capacity(messages.len() + 1);
    history.push(json!({ "role": "system", "content": "" }));
    history.extend_from_slice(messages);

    let Some(cut) = select_latest_turn(&history) else {
        return Ok(None);
    };
    if estimate_tokens(&history[1..cut]) < MIN_REPLACED {
        return Ok(None);
    }

    let next = compact_at(client, config, &history, cut, cancel, false).await?;
    Ok(Some(next[1..].to_vec()))
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
        "<compacted-summary>\n{}\n</compacted-summary>",
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

fn content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Flatten the compacted range into plain text for the summarizer.
fn render(messages: &[Value]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = role(message).unwrap_or("unknown");
        let text = content_text(message);

        match role {
            "assistant" => {
                out.push_str("[assistant] ");
                out.push_str(text.trim());
                out.push('\n');
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        let arguments = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        out.push_str("  calls ");
                        out.push_str(name);
                        out.push('(');
                        out.push_str(arguments);
                        out.push_str(")\n");
                    }
                }
            }
            "tool" => {
                out.push_str("[tool result] ");
                out.push_str(text.trim());
                out.push('\n');
            }
            _ => {
                out.push('[');
                out.push_str(role);
                out.push_str("] ");
                out.push_str(text.trim());
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

async fn summarize(
    client: &reqwest::Client,
    config: &AppConfig,
    rendered: &str,
    cancel: &Cancel,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let body = json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": SUMMARIZER_PROMPT },
            {
                "role": "user",
                "content": format!("Conversation to compress:\n\n{rendered}")
            }
        ],
        "stream": false,
        "temperature": 0.2,
        "max_tokens": SUMMARY_MAX_TOKENS,
    });

    let mut request = client.post(&url).json(&body).timeout(SUMMARY_TIMEOUT);
    if !config.api_key.is_empty() {
        request = request.bearer_auth(&config.api_key);
    }

    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Stopped.".into()),
        response = request.send() => response
            .map_err(|err| format!("Could not reach {}: {err}", config.base_url))?,
    };

    if !response.status().is_success() {
        let status = response.status();
        let detail = cap(response.text().await.unwrap_or_default(), 2_000);
        return Err(format!("Summary request failed ({status}): {detail}"));
    }

    let value: Value = response
        .json()
        .await
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

fn note_fallback(reason: &str) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] summary failed ({reason}); dropped old history instead");
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

    /// One-shot OpenAI-compatible endpoint that answers a summary request and
    /// records its body.
    fn summary_server() -> (String, Arc<StdMutex<String>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(StdMutex::new(String::new()));
        let seen_by_server = seen.clone();
        let server = thread::spawn(move || {
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
                        let body = String::from_utf8_lossy(&raw[head + 4..head + 4 + length]);
                        *seen_by_server.lock().unwrap() = body.to_string();
                        break;
                    }
                }
            }
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"Goal: ship compaction."}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
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
    fn renders_tool_calls_and_results_for_the_summarizer() {
        let messages = vec![
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
        let text = render(&messages);
        assert!(text.contains("read_file"));
        assert!(text.contains("{\"path\":\"a\"}"));
        assert!(text.contains("file body"));
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

        let next = compact(&client, &config, &messages, &cancel)
            .await
            .unwrap()
            .expect("compaction should apply");

        server.join().unwrap();
        assert!(next.len() < messages.len());
        assert_eq!(next[0]["role"], "system");
        assert!(next.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("ship compaction."))
        }));
        assert_eq!(next.last().unwrap(), &last);

        let request = seen.lock().unwrap();
        assert!(request.contains("Conversation to compress"));
        assert!(request.contains("request 0"));
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

        let next = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
            .await
            .unwrap()
            .expect("compaction should apply");

        server.join().unwrap();
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

        let next = compact_messages(&reqwest::Client::new(), &config, &messages, &cancel)
            .await
            .unwrap()
            .expect("compaction should apply");

        server.join().unwrap();
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
