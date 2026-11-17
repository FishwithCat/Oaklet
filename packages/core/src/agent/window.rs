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

#[derive(Default)]
struct CompactionTrace {
    archived: Vec<Value>,
    summary: Option<Value>,
}

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
    let mut trace = CompactionTrace::default();
    // A forced pass can fail after a normal pass already shrank the history.
    // Report the size the run started from instead of a zero gauge.
    let before =
        estimate_tokens(messages) + crate::context::estimate_text(&tools.schemas().to_string());
    let outcome = compact_before_request(
        client,
        config,
        tools,
        cancel,
        messages,
        meter,
        &mut trace,
    )
    .await;
    let after =
        estimate_tokens(messages) + crate::context::estimate_text(&tools.schemas().to_string());
    if after != before {
        if let Some(checkpoint) = trace.summary.take() {
            ctx.set_summary(checkpoint);
        }
        ctx.mark_compacted(before, after);
        ctx.record_archived(messages).await?;
        ensure_history_tool(tools, ctx).await;
        meter.set_schema_tokens(crate::context::estimate_text(&tools.schemas().to_string()));
        ctx.mark_compacted(
            before,
            estimate_tokens(messages) + crate::context::estimate_text(&tools.schemas().to_string()),
        );
    }
    if meter.pressure(messages) > config.context_window {
        return Err(
            "The current request and tool schemas exceed the context window after compaction."
                .into(),
        );
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
    trace: &mut CompactionTrace,
) -> Result<Option<usize>, String> {
    let trigger = compaction::trigger_at(config.context_window);
    let schemas = crate::context::estimate_text(&tools.schemas().to_string());
    meter.set_schema_tokens(schemas);
    let system = messages
        .first()
        .map(crate::context::estimate_message)
        .unwrap_or(0);
    if schemas + system >= config.context_window {
        return Err("The system prompt and tool schemas already exceed the context window; reduce tools or raise the context window.".into());
    }
    let pressure = meter.prepare(messages);
    if pressure <= trigger {
        return Ok(None);
    }

    let mut compacted_any = false;
    if let Some(compacted) =
        compaction::compact(client, config, messages, pressure, tools, cancel).await?
    {
        compacted_any = true;
        if let Some(checkpoint) = compacted.summary.clone() {
            trace.summary = Some(checkpoint);
        }
        note_compaction(
            estimate_tokens(messages) + schemas,
            estimate_tokens(&compacted.messages) + schemas,
        );
        trace.archived.extend(compacted.replaced);
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
        if let Some(checkpoint) = compacted.summary.clone() {
            trace.summary = Some(checkpoint);
        }
        note_compaction(
            estimate_tokens(messages) + schemas,
            estimate_tokens(&compacted.messages) + schemas,
        );
        trace.archived.extend(compacted.replaced);
        *messages = compacted.messages;
        let _ = meter.prepare(messages);
    }

    if meter.pressure(messages) > config.context_window {
        return Err("The current request and tool schemas could not fit the context window after compaction.".into());
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
        let mut trace = CompactionTrace::default();
        let (_handle, cancel) = CancelHandle::new();

        compact_before_request(
            &reqwest::Client::new(),
            &config,
            &ToolRegistry::new(),
            &cancel,
            &mut messages,
            &mut meter,
            &mut trace,
        )
        .await
        .unwrap();

        assert!(trace.archived.iter().any(|message| {
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
                .is_some_and(|text| text.contains("request 0"))
        }));
    }
}

#[cfg(test)]
mod schema_budget_tests {
    use super::*;
    use crate::compaction::test_support::{assistant, large_tool_registry, system, user};
    use crate::tools::CancelHandle;

    #[tokio::test]
    async fn large_schemas_trigger_preflight_even_when_history_is_below_trigger() {
        let tools = large_tool_registry();
        let schemas = crate::context::estimate_text(&tools.schemas().to_string());
        let config = AppConfig {
            context_window: 32768,
            base_url: "invalid://summary".into(),
            ..AppConfig::default()
        };
        let budget = compaction::trigger_at(config.context_window);
        let text = "x".repeat((budget - schemas / 2) * 2);
        let mut messages = vec![
            system(),
            user(&text),
            assistant(&text),
            user("current request"),
        ];
        assert!(estimate_tokens(&messages) < budget);
        assert!(estimate_tokens(&messages) + schemas > budget);
        let mut meter = Meter::default();
        let mut trace = CompactionTrace::default();
        let (_handle, cancel) = CancelHandle::new();
        let compacted = compact_before_request(
            &reqwest::Client::new(),
            &config,
            &tools,
            &cancel,
            &mut messages,
            &mut meter,
            &mut trace,
        )
        .await
        .unwrap();
        assert!(compacted.is_some());
        assert!(!trace.archived.is_empty());
        assert_eq!(
            meter.pressure(&messages),
            estimate_tokens(&messages) + schemas
        );
        assert!(meter.pressure(&messages) <= budget);
    }

    #[tokio::test]
    async fn oversized_fixed_cost_reports_an_actionable_error_without_a_summary_request() {
        let config = AppConfig {
            context_window: 4096,
            base_url: "invalid://summary".into(),
            ..AppConfig::default()
        };
        let mut messages = vec![system(), user("hello")];
        let (_handle, cancel) = CancelHandle::new();
        let error = compact_before_request(
            &reqwest::Client::new(),
            &config,
            &large_tool_registry(),
            &cancel,
            &mut messages,
            &mut Meter::default(),
            &mut CompactionTrace::default(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("tool schemas"));
        assert!(error.contains("reduce tools"));
    }

    #[tokio::test]
    async fn fixed_cost_over_trigger_but_under_window_is_still_sent() {
        let config = AppConfig {
            context_window: 8192,
            base_url: "invalid://summary".into(),
            ..AppConfig::default()
        };
        let mut messages = vec![system(), user("hello")];
        let tools = large_tool_registry();
        let (_handle, cancel) = CancelHandle::new();
        let compacted = compact_before_request(
            &reqwest::Client::new(),
            &config,
            &tools,
            &cancel,
            &mut messages,
            &mut Meter::default(),
            &mut CompactionTrace::default(),
        )
        .await
        .unwrap();
        assert!(compacted.is_none());
        assert!(
            estimate_tokens(&messages)
                + crate::context::estimate_text(&tools.schemas().to_string())
                <= config.context_window
        );
    }
}
