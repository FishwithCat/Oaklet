//! Keeping the next request inside the configured context window.
//!
//! Pressure is reported every step; a request that would exceed the trigger is
//! compacted first so a run merely near the limit keeps a comfortable tail.

use serde_json::Value;

use super::ensure_history_tool;
use crate::AppConfig;
use crate::chat::RunContext;
use crate::compaction;
use crate::context::{Meter, estimate_tokens};
use crate::tools::{Cancel, ToolRegistry};

/// Tell the debug log and the UI how large the next request will be.
pub(super) fn report(ctx: &RunContext, config: &AppConfig, meter: &Meter, messages: &[Value]) {
    let pressure = meter.pressure(messages);
    note_pressure(pressure);
    ctx.context(pressure, config.context_window);
}

/// Compact before a request when it would exceed the trigger.
///
/// The result is recorded on `ctx` and the history is archived so the next
/// request is sent with the smaller transcript.
pub(super) async fn make_room(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    meter: &mut Meter,
    cancel: &Cancel,
    ctx: &RunContext,
) -> Result<(), String> {
    let mut replaced = Vec::new();
    let outcome = compact_before_request(
        client,
        config,
        tools,
        cancel,
        messages,
        meter,
        &mut replaced,
    )
    .await;
    let before = outcome.as_ref().ok().copied().flatten().unwrap_or(0);
    if !replaced.is_empty() {
        ctx.mark_compacted(before);
        ctx.record_archived(&replaced);
        ensure_history_tool(tools, ctx);
    }
    outcome.map(|_| ())
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
) -> Result<Option<usize>, String> {
    let trigger = compaction::trigger_at(config.context_window);
    let pressure = meter.prepare(messages);
    if pressure <= trigger {
        return Ok(None);
    }

    let mut compacted_any = false;
    if let Some(compacted) =
        compaction::compact(client, config, messages, pressure, tools, cancel).await?
    {
        compacted_any = true;
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
        compacted_any = true;
        note_compaction(
            estimate_tokens(messages),
            estimate_tokens(&compacted.messages),
        );
        archived.extend(compacted.replaced);
        *messages = compacted.messages;
        let _ = meter.prepare(messages);
    }

    Ok(compacted_any.then_some(pressure))
}

/// Print context pressure while developing; release builds stay quiet.
fn note_pressure(pressure: usize) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] context pressure ~{pressure} tokens");
    }
}

pub(super) fn note_compaction(before: usize, after: usize) {
    if cfg!(debug_assertions) {
        eprintln!("[oaklet] compacted context ~{before} -> ~{after} tokens");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::CancelHandle;
    use serde_json::json;

    #[tokio::test]
    async fn preflight_compacts_saved_history_before_the_first_request() {
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
}
