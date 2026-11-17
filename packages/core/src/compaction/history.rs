//! Select safe history boundaries and trim oversized tool results.

use serde_json::{Value, json};
use std::collections::BTreeSet;

use crate::context::{estimate_message, estimate_tokens};
use crate::repeat::is_reminder;
use crate::tools::spill_pointer;

/// Normal pressure keeps more detail than overflow recovery, while fitting
/// twenty typical tool results in a 32k window without a summary request.
pub(super) const PRESSURE_TOOL_OUTPUT_CHARS: usize = 4_500;

pub(super) const PRESSURE_TOOL_OUTPUT_HEAD_CHARS: usize = 3_000;

pub(super) const PRESSURE_TOOL_OUTPUT_TAIL_CHARS: usize = 1_000;

/// Tool output kept during overflow recovery. The normal pipeline already
/// caps one result at 12k characters, but a long run can accumulate enough of
/// them to overflow anyway.
pub(super) const FORCE_TOOL_OUTPUT_CHARS: usize = 2_000;

pub(super) const FORCE_TOOL_OUTPUT_HEAD_CHARS: usize = 1_400;

pub(super) const FORCE_TOOL_OUTPUT_TAIL_CHARS: usize = 400;

const FORCE_TOOL_OUTPUT_MARKER: &str = "\n\n[... tool output trimmed for context recovery; \
     read the archived original with history_read if the middle matters ...]\n\n";

/// First text part of the synthetic user turn that carries tool images.
/// `agent::messages::image_message` uses the same sentence.
pub(super) const TOOL_IMAGE_NOTICE: &str = "Images returned by the previous tool call(s).";

/// L0: shrink tool results in place without touching the conversational order.
pub(super) fn trim_tool_outputs(
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

/// Cut immediately before the latest user turn.
///
/// Manual compaction replaces every older turn with one checkpoint and keeps
/// the current turn verbatim. Starting the tail at a user message also keeps
/// the turn's tool-call/result block intact.
pub(super) fn select_latest_turn(messages: &[Value]) -> Option<usize> {
    if messages.len() < 3 {
        return None;
    }
    let cut = latest_user(messages);
    (cut >= 2).then_some(cut)
}

/// Index of the latest real user message, preferring it over loop-guard advice
/// and the synthetic user turn that carries tool images.
pub(super) fn latest_user(messages: &[Value]) -> usize {
    messages
        .iter()
        .rposition(|message| {
            role(message) == Some("user")
                && !is_reminder(message)
                && !is_tool_image_message(message)
        })
        .or_else(|| {
            messages
                .iter()
                .rposition(|message| role(message) == Some("user"))
        })
        .unwrap_or(1)
}

fn is_tool_image_message(message: &Value) -> bool {
    if role(message) != Some("user") {
        return false;
    }
    match message.get("content") {
        Some(Value::Array(parts)) => parts
            .iter()
            .find_map(|part| part.get("text").and_then(Value::as_str))
            .is_some_and(|text| text.trim_start().starts_with(TOOL_IMAGE_NOTICE)),
        _ => false,
    }
}

/// Pick the earliest cut whose tail fits `retain`, without starting the tail
/// with an orphaned `tool` message and without touching the current request.
///
/// A `tool` message is only valid directly after the assistant message that
/// requested it, so a cut that would leave one at the front of the tail has to
/// move forward and keep the whole tool block.
pub(super) fn select_cut(messages: &[Value], retain: usize, min_replaced: usize) -> Option<usize> {
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

pub(super) fn role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

#[cfg(test)]
mod tests;
