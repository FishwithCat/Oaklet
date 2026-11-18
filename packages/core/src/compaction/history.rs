//! Identity-based selection at complete tool-batch boundaries.
use crate::context::{estimate_message, estimate_tokens, is_request};
use crate::tools::spill_pointer;
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) struct Plan {
    pub cut: usize,
    pub request: Option<usize>,
    pub candidates: Vec<usize>,
}

impl Plan {
    pub fn replaced_tokens(&self, messages: &[Value]) -> usize {
        messages[..self.cut]
            .iter()
            .enumerate()
            .filter(|(index, message)| message["role"] != "system" && Some(*index) != self.request)
            .map(|(_, message)| estimate_message(message))
            .sum()
    }
}

fn boundaries(messages: &[Value]) -> Result<Vec<usize>, String> {
    let start = usize::from(
        messages
            .first()
            .is_some_and(|message| message["role"] == "system"),
    );
    let latest = messages
        .iter()
        .rposition(|message| message["role"] == "assistant")
        .unwrap_or(messages.len());
    let mut pending = HashSet::new();
    let mut cuts = Vec::new();
    for (index, message) in messages.iter().enumerate().skip(start) {
        if index > start
            && index <= latest
            && pending.is_empty()
            && !matches!(
                message["oaklet_source"].as_str(),
                Some("tool_images" | "reminder")
            )
            && message["role"] != "tool"
        {
            cuts.push(index);
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let id = call["id"].as_str().ok_or("A tool call has no identity.")?;
                if !pending.insert(id) {
                    return Err("Duplicate pending tool call.".into());
                }
            }
        }
        if message["role"] == "tool" {
            let id = message["tool_call_id"]
                .as_str()
                .ok_or("A tool result has no identity.")?;
            if !pending.remove(id) {
                return Err("An orphaned tool result prevents safe compaction.".into());
            }
        }
    }
    if !pending.is_empty() {
        return Err("Finish the pending tool batch before compacting.".into());
    }
    Ok(cuts)
}

fn useful(messages: &[Value], cut: usize, request: Option<usize>) -> bool {
    let start = usize::from(
        messages
            .first()
            .is_some_and(|message| message["role"] == "system"),
    );
    (start..cut).any(|index| Some(index) != request)
}

pub(super) fn plan(messages: &[Value], retain: usize) -> Result<Option<Plan>, String> {
    let request = messages.iter().rposition(is_request);
    let cuts = boundaries(messages)?;
    let candidates: Vec<_> = cuts
        .into_iter()
        .filter(|cut| useful(messages, *cut, request))
        .collect();
    let cut = candidates
        .iter()
        .copied()
        .find(|cut| estimate_tokens(&messages[*cut..]) <= retain)
        .or_else(|| candidates.last().copied());
    Ok(cut.map(|cut| Plan {
        cut,
        request,
        candidates: candidates
            .into_iter()
            .take_while(|candidate| *candidate <= cut)
            .collect(),
    }))
}

/// A separate deterministic operation: callers apply it before attempting a summary.
pub(crate) fn trim_tool_outputs(messages: &[Value]) -> Option<Vec<Value>> {
    let mut changed = false;
    let next = messages.iter().map(|message| {
        if message["role"] != "tool" { return message.clone(); }
        let Some(text) = message["content"].as_str().filter(|text| text.chars().count() > 4_500) else { return message.clone(); };
        let marker = match spill_pointer(text) {
            Some(path) => format!("\n\n[... tool output trimmed for context recovery; full output saved to: {path}. Use read_file with offset and limit to page through it.]\n\n"),
            None => "\n\n[... tool output trimmed; use history_read for the original ...]\n\n".into(),
        };
        let mut trimmed = message.clone();
        let text = format!("{}{}{}", text.chars().take(3_000).collect::<String>(), marker,
            text.chars().skip(text.chars().count() - 1_000).collect::<String>());
        trimmed["content"] = json!(text);
        changed = true;
        trimmed
    }).collect();
    changed.then_some(next)
}
