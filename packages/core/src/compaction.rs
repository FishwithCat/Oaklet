//! Trim tool output before replacing old conversation with a checkpoint when
//! the prompt approaches the configured context window.
//!
//! The system prompt and newest user request stay untouched; older conversation
//! and completed tool work can become one `user` checkpoint so the run can
//! continue instead of failing with a context-length error. The history is a plain array here, so a failed
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

mod checkpoint;
mod history;
mod recovery;
mod summary;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use serde_json::{Value, json};

use crate::AppConfig;
use crate::context::estimate_tokens;
use crate::tools::{Cancel, ToolRegistry};

use checkpoint::{assemble, fallback_summary};
use history::{
    PRESSURE_TOOL_OUTPUT_CHARS, PRESSURE_TOOL_OUTPUT_HEAD_CHARS, PRESSURE_TOOL_OUTPUT_TAIL_CHARS,
    select_cut, select_latest_turn, trim_tool_outputs,
};
pub use recovery::force;
use summary::{prefix_fits, summarize};

/// Room kept for the response and for the next round of tool output.
const OUTPUT_RESERVE: usize = 32_768;

/// Below this, a summary would cost more than the history it replaces.
const MIN_REPLACED: usize = 4_000;

/// A compacted history plus the original messages it replaced.
pub struct Compacted {
    pub messages: Vec<Value>,
    pub replaced: Vec<Value>,
    pub summary: Option<Value>,
}

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
            summary: None,
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
        compact_at(
            client,
            config,
            messages,
            cut,
            Some(tools),
            cancel,
            CompactMode::Auto,
        )
        .await?,
    ))
}

/// How compaction recovers when the model summary is unavailable or too large.
#[derive(Clone, Copy)]
enum CompactMode {
    /// Keep an automatic run fitting the window; fall back to a digest.
    Auto,
    /// Report the failure and leave the history intact.
    Manual,
    /// Keep the live request verbatim and out of the mechanical digest.
    LongTurn(usize),
}

/// Summarize `messages[1..cut]` and replace that range with one checkpoint.
async fn compact_at(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    cut: usize,
    tools: Option<&ToolRegistry>,
    cancel: &Cancel,
    mode: CompactMode,
) -> Result<Compacted, String> {
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let manual = matches!(mode, CompactMode::Manual);
    let skip_request = match mode {
        CompactMode::LongTurn(request) => Some(request),
        _ => None,
    };
    let original = messages.to_vec();
    let config_for_plan = config.clone();
    let schema_tokens = tools
        .map(|tools| crate::context::estimate_text(&tools.schemas().to_string()))
        .unwrap_or(0);
    let (planned, omit_tools, trimmed_indices) = tauri::async_runtime::spawn_blocking(move || {
        let (mut trimmed, mut indices) = trim_tool_outputs(
            &original,
            PRESSURE_TOOL_OUTPUT_CHARS,
            PRESSURE_TOOL_OUTPUT_HEAD_CHARS,
            PRESSURE_TOOL_OUTPUT_TAIL_CHARS,
        );
        if manual {
            trimmed[cut..].clone_from_slice(&original[cut..]);
            indices.retain(|index| *index < cut);
        }
        let fits = |history: &[Value], schemas: usize| {
            summary::prefix_tokens(history, cut).saturating_add(schemas)
                <= config_for_plan
                    .context_window
                    .saturating_sub(summary::output_limit(&config_for_plan))
        };
        if fits(&original, schema_tokens) {
            (original, false, Default::default())
        } else if fits(&trimmed, schema_tokens) {
            (trimmed, false, indices)
        } else if fits(&original, 0) {
            (original, true, Default::default())
        } else {
            (trimmed, true, indices)
        }
    })
    .await
    .map_err(|error| format!("Could not prepare compaction: {error}"))?;
    let tools = if omit_tools { None } else { tools };
    let mut replaced_indices: std::collections::BTreeSet<_> = (1..cut).collect();
    replaced_indices.extend(trimmed_indices);
    let replaced = replaced_indices
        .into_iter()
        .filter(|&index| !crate::store::is_checkpoint(&messages[index]))
        .map(|index| messages[index].clone())
        .collect::<Vec<_>>();

    if prefix_fits(&planned, cut, config, tools) {
        match summarize(client, config, &planned[..cut], tools, cancel).await {
            Ok(summary) => {
                let compacted = assemble(&planned, cut, &summary);
                // Count checkpoint framing and the archive notice in the shrink check.
                if estimate_tokens(&compacted) < estimate_tokens(messages) {
                    return Ok(Compacted {
                        summary: Some(compacted[1].clone()),
                        messages: compacted,
                        replaced,
                    });
                }
                note_fallback("checkpoint would not shrink the history");
                if manual {
                    let replaced_tokens = estimate_tokens(&replaced);
                    return Err(format!(
                        "The checkpoint would not be smaller than the history it replaced ({replaced_tokens} tokens); nothing was compacted."
                    ));
                }
            }
            Err(err) => {
                if cancel.is_cancelled() {
                    return Err("Stopped.".into());
                }
                note_fallback(&err);
                if manual {
                    return Err(err);
                }
            }
        }
    } else {
        note_fallback("summary prefix would not fit");
    }
    let original = messages.to_vec();
    let compacted = tauri::async_runtime::spawn_blocking(move || {
        assemble(
            &planned,
            cut,
            &fallback_summary(&original, cut, skip_request),
        )
    })
    .await
    .map_err(|error| format!("Could not build checkpoint: {error}"))?;
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    Ok(Compacted {
        summary: Some(compacted[1].clone()),
        messages: compacted,
        replaced,
    })
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
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let mut history = Vec::with_capacity(messages.len() + 1);
    history.push(json!({ "role": "system", "content": "" }));
    history.extend_from_slice(messages);

    let Some(cut) = select_latest_turn(&history) else {
        return Ok(None);
    };
    if estimate_tokens(&history[1..cut]) < MIN_REPLACED {
        return Ok(None);
    }

    let compacted = compact_at(
        client,
        config,
        &history,
        cut,
        None,
        cancel,
        CompactMode::Manual,
    )
    .await?;
    Ok(Some(Compacted {
        summary: compacted.summary,
        messages: compacted.messages[1..].to_vec(),
        replaced: compacted.replaced,
    }))
}

fn note_fallback(reason: &str) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] summary failed ({reason}); trying a mechanical checkpoint");
    }
}
