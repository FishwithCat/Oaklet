//! One validated prefix replacement shared by automatic, manual and overflow triggers.
mod history;
mod summary;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use crate::tools::{Cancel, ToolRegistry};
use crate::{AppConfig, context};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub(crate) enum Trigger {
    Automatic,
    Manual,
    Overflow,
}

pub fn trigger_at(window: usize) -> usize {
    context::input_limit(window).min(window - window.div_ceil(5))
}

pub fn retain_at(window: usize) -> usize {
    (window / 6).max(2_000).min(window / 2)
}

pub(crate) use history::trim_tool_outputs;

/// The borrowed input cannot change while a summary is generated. The store
/// separately validates identities/version before committing its projection.
pub async fn compact(
    client: &reqwest::Client,
    config: &AppConfig,
    messages: &[Value],
    tools: &ToolRegistry,
    trigger: Trigger,
    cancel: &Cancel,
) -> Result<Option<Vec<Value>>, String> {
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let retain = match trigger {
        Trigger::Automatic => retain_at(config.context_window),
        Trigger::Manual | Trigger::Overflow => 2_000.min(retain_at(config.context_window)),
    };
    let Some(mut plan) = history::plan(messages, retain)? else {
        return Ok(None);
    };
    // Prefix cost is monotone; find the largest safe prefix without repeatedly
    // scanning and cloning every earlier candidate in a long conversation.
    let mut low = 0;
    let mut high = plan.candidates.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if summary::prefix_fits(messages, plan.candidates[middle], config, None)? {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    if low == 0 {
        return Err("No complete context block fits the summary model's window; increase its context window or reduce the input. History was preserved.".into());
    }
    plan.cut = plan.candidates[low - 1];
    let replay_tools =
        summary::prefix_fits(messages, plan.cut, config, Some(tools))?.then_some(tools);
    let summary =
        summary::summarize(client, config, &messages[..plan.cut], replay_tools, cancel).await?;
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let start = usize::from(
        messages
            .first()
            .is_some_and(|message| message["role"] == "system"),
    );
    let mut next = messages[..start].to_vec();
    next.push(context::identify(crate::store::checkpoint(json!({
        "role":"user", "content": format!("<compacted-summary>\n{}\n</compacted-summary>\n\nEarlier original messages and images remain available through history_read.", summary.trim())
    })), context::MessageId::Checkpoint));
    if let Some(request) = plan.request.filter(|index| *index < plan.cut) {
        next.push(messages[request].clone());
    }
    next.extend_from_slice(&messages[plan.cut..]);
    if context::request_tokens(&next, &tools.schemas())
        >= context::request_tokens(messages, &tools.schemas())
    {
        return Err("The summary did not shrink the context; history was preserved.".into());
    }
    Ok(Some(next))
}
