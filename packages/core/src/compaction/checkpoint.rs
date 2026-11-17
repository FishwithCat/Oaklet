//! Build checkpoints and bounded mechanical digests without changing the retained tail.

use serde_json::{Value, json};

use super::history::role;
use crate::context::estimate_tokens;
use crate::tools::cap;

/// Bound new log excerpts; a prior checkpoint is retained in full during fallback.
const DIGEST_TEXT_CHARS: usize = 4_000;

const DIGEST_TOOL_CHARS: usize = 500;

const PROGRESS_START: &str = "\n\n<checkpoint-progress>\n";

const PROGRESS_END: &str = "\n</checkpoint-progress>";

const REQUESTS_START: &str = "\n\n<checkpoint-requests>\n";

const REQUESTS_END: &str = "\n</checkpoint-requests>";

const ARCHIVE_NOTICE: &str = "Earlier messages replaced by this checkpoint are archived; call \
history_read if you need their exact commands, errors, or file contents.";

/// Last resort when neither a model summary nor a mechanical digest shrinks history.
pub(super) const DROPPED: &str = "The earlier part of this conversation was dropped so the run could stay \
within the context window. The original messages are archived: use history_read when you need an \
exact earlier command, error, or file content instead of assuming what happened before.";

/// Deterministic fallback; include the checkpoint framing in the shrink check.
pub(super) fn fallback_summary(
    messages: &[Value],
    cut: usize,
    skip_request: Option<usize>,
) -> String {
    let skip = skip_request.map(|index| index.saturating_sub(1));
    let digest = mechanical_digest(&messages[1..cut], skip);
    if !digest.is_empty()
        && estimate_tokens(&assemble(messages, cut, &digest)) < estimate_tokens(messages)
    {
        digest
    } else {
        DROPPED.to_string()
    }
}

pub(super) fn mechanical_digest(messages: &[Value], skip: Option<usize>) -> String {
    let mut entries = Vec::new();
    let mut checkpoints = Vec::new();
    let mut requests = Vec::new();
    let mut previous_progress = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if Some(index) == skip {
            continue;
        }
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
                    let (summary, request) = checkpoint_parts(&content);
                    if !summary.is_empty() {
                        let (mut prior, progress) =
                            summary.split_once(PROGRESS_START).unwrap_or((&summary, ""));
                        if let Some((base, encoded)) = prior.split_once(REQUESTS_START)
                            && let Some(encoded) = encoded.strip_suffix(REQUESTS_END)
                            && let Ok(saved) = serde_json::from_str::<Vec<String>>(encoded)
                        {
                            requests.extend(saved);
                            prior = base;
                        }
                        checkpoints.push(prior.to_string());
                        let progress = progress.strip_suffix(PROGRESS_END).unwrap_or(progress);
                        // Legacy excerpts mixed user requirements with logs; keep that small block intact.
                        if progress
                            .split("\n\n")
                            .any(|entry| entry.starts_with("user: "))
                        {
                            requests.push(progress.to_string());
                        } else if !progress.is_empty() {
                            previous_progress.push(progress.to_string());
                        }
                    }
                    if !request.trim().is_empty() && !checkpoints.is_empty() {
                        requests.push(request);
                        String::new()
                    } else {
                        request
                    }
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
    let mut checkpoint = checkpoints.join("\n\n");
    let unique_requests = bounded_requests(requests);
    if !unique_requests.is_empty() {
        checkpoint.push_str(REQUESTS_START);
        checkpoint.push_str(&serde_json::to_string(&unique_requests).unwrap());
        checkpoint.push_str(REQUESTS_END);
    }
    let budget = DIGEST_TEXT_CHARS;
    // New progress gets first claim; older progress keeps only its tail.
    let mut digest_entries = Vec::new();
    let old = previous_progress.join("\n\n");
    if !old.is_empty() {
        digest_entries.push(crate::spill::preview(&old, 0, usize::MAX, 0, budget / 4).tail);
    }
    digest_entries.extend(entries);
    let digest = digest_entries.join("\n\n");
    let recent = if digest.chars().count() <= budget {
        digest
    } else {
        let preview = crate::spill::preview(&digest, usize::MAX, 0, budget * 3 / 5, budget / 4);
        format!(
            "{}\n\n[... digest middle omitted; use history_read for the original ...]\n\n{}",
            preview.head, preview.tail
        )
    };
    if checkpoint.is_empty() {
        recent
    } else if recent.is_empty() {
        checkpoint
    } else {
        format!("{checkpoint}{PROGRESS_START}{recent}{PROGRESS_END}")
    }
}

/// Deduplicate folded requests and keep the goal plus the newest ones.
///
/// This block is re-serialized into every later checkpoint, so an unbounded
/// list makes each fallback larger than the history it replaces. Individual
/// requests are capped too; the originals stay in the archive.
fn bounded_requests(requests: Vec<String>) -> Vec<String> {
    const REQUEST_MAX_CHARS: usize = 1_000;
    let mut unique: Vec<String> = Vec::new();
    for request in requests {
        let request = cap(request, REQUEST_MAX_CHARS);
        if !unique.contains(&request) {
            unique.push(request);
        }
    }
    while unique.len() > 2
        && unique
            .iter()
            .map(|request| request.chars().count() + 4)
            .sum::<usize>()
            > DIGEST_TEXT_CHARS
    {
        unique.remove(1);
    }
    unique
}

/// Keep prior checkpoints outside the log budget, and retain any merged user request.
pub(super) fn checkpoint_parts(mut text: &str) -> (String, String) {
    let mut summaries = Vec::new();
    let mut plain = String::new();
    while let Some((before, rest)) = text.split_once("<compacted-summary>") {
        let Some((summary, after)) = rest.split_once("</compacted-summary>") else {
            break;
        };
        plain.push_str(before);
        summaries.push(summary.trim());
        text = after;
        if let Some(after_notice) = text.trim_start().strip_prefix(ARCHIVE_NOTICE) {
            text = after_notice.strip_prefix("\n\n").unwrap_or(after_notice);
        }
    }
    plain.push_str(text);
    (summaries.join("\n\n"), plain)
}

/// Rewrite the history as: system prompt, checkpoint, untouched tail.
///
/// When the tail starts with a user message the checkpoint is merged into it,
/// which avoids two consecutive user turns that some providers reject.
pub(super) fn assemble(messages: &[Value], cut: usize, summary: &str) -> Vec<Value> {
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

pub(super) fn prepend_text(message: &mut Value, prefix: &str) {
    match &mut message["content"] {
        Value::String(text) => *text = format!("{prefix}\n\n{text}"),
        Value::Array(parts) => parts.insert(0, json!({ "type": "text", "text": prefix })),
        other => *other = json!(prefix),
    }
}

#[cfg(test)]
mod tests;
