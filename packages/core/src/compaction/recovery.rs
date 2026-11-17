//! Recover from provider overflow while preserving the current request and original messages.

use serde_json::{Value, json};

use super::checkpoint::{DROPPED, assemble, checkpoint_parts, mechanical_digest, prepend_text};
use super::history::{
    FORCE_TOOL_OUTPUT_CHARS, FORCE_TOOL_OUTPUT_HEAD_CHARS, FORCE_TOOL_OUTPUT_TAIL_CHARS,
    latest_user, role, select_cut, trim_tool_outputs,
};
use super::{CompactMode, Compacted, MIN_REPLACED, compact_at, retain_at};
use crate::AppConfig;
use crate::context::{estimate_message, estimate_tokens};
use crate::store::is_checkpoint;
use crate::tools::{Cancel, ToolRegistry};

/// Overflow recovery keeps only a small recent tail. The summary carries the
/// rest, and the current request is never cut. Normal compaction retains about
/// a sixth of the window; a provider rejection means that budget was too big.
const FORCE_RETAIN_TOKENS: usize = 2_000;

/// Retain used when the provider has already rejected the prompt for size.
///
/// Normal compaction can leave a tail that is still too large when the real
/// window is smaller than the configured one. Keep only a small tail so the
/// retry has real room instead of a token shave.
fn force_retain_at(window: usize) -> usize {
    let retain = retain_at(window);
    (retain / 2).clamp(1_000, FORCE_RETAIN_TOKENS).min(retain)
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
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
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
        let merged = cut < next.len() && role(&next[cut]) == Some("user");
        next = compact_at(
            client,
            config,
            &next,
            cut,
            Some(tools),
            cancel,
            CompactMode::Auto,
        )
        .await?
        .messages;
        // Trim/summarize/drop rewrite or remove messages; collect their original
        // indices so the archive always holds the untrimmed text.
        replaced.extend(1..cut);
        origins = reassemble_origins(&origins, cut, merged);
    }

    while estimate_tokens(&next) > target {
        let Some((dropped, removed)) = drop_oldest_turn(&next) else {
            break;
        };
        replaced.extend(origins[removed.clone()].iter().flatten().copied());
        origins = reassemble_origins(&origins, removed.end, true);
        next = dropped;
    }

    // Keep the request verbatim while checkpointing completed work inside a long turn.
    if estimate_tokens(&next) > target {
        let current = latest_user(&next);
        let latest_block =
            (current + 1..next.len()).rfind(|&index| role(&next[index]) == Some("assistant"));
        let mut suffix = estimate_tokens(&next);
        let cut = next
            .iter()
            .enumerate()
            .find_map(|(index, message)| {
                let fits =
                    index > current + 1 && role(message) == Some("assistant") && suffix <= target;
                suffix = suffix.saturating_sub(estimate_message(message));
                fits.then_some(index)
            })
            .or_else(|| latest_block.filter(|&index| index > current + 1));
        if let Some(cut) = cut {
            let mut request = next[current].clone();
            match &mut request["content"] {
                Value::String(text) => *text = checkpoint_parts(text).1,
                Value::Array(parts) => {
                    parts.retain_mut(|part| {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            let (summary, plain) = checkpoint_parts(text);
                            if !summary.is_empty() {
                                if plain.is_empty() {
                                    return false;
                                }
                                part["text"] = json!(plain);
                            }
                        }
                        true
                    });
                }
                _ => {}
            }
            let mut compacted = compact_at(
                client,
                config,
                &next,
                cut,
                Some(tools),
                cancel,
                CompactMode::LongTurn(current),
            )
            .await?
            .messages;
            prepend_text(&mut request, compacted[1]["content"].as_str().unwrap());
            compacted[1] = request;
            if estimate_tokens(&compacted) < estimate_tokens(&next) {
                replaced.extend(origins[1..cut].iter().flatten().copied());
                next = compacted;
            }
        }
    }

    if estimate_tokens(&next) >= estimate_tokens(messages) {
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

    // Fold the kept prefix into the checkpoint so the persisted tail stays contiguous.
    let kept = mechanical_digest(&messages[1..start], None);
    let summary = if kept.is_empty() {
        DROPPED.to_string()
    } else if kept.contains(DROPPED) {
        kept
    } else {
        format!("{kept}\n\n{DROPPED}")
    };
    Some((assemble(messages, end, &summary), 1..end))
}

#[cfg(test)]
mod tests;
