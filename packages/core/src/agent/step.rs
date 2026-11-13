//! One model request, including the single context-overflow retry.
//!
//! A provider can reject a prompt that was under the local trigger; the only
//! recovery is to compact harder and retry once before reporting the error.

use serde_json::Value;

use super::ToolCall;
use super::ensure_history_tool;
use super::model::stream_step;
use super::window::note_compaction;
use crate::AppConfig;
use crate::chat::RunContext;
use crate::compaction;
use crate::context::estimate_tokens;
use crate::tools::{Cancel, ToolRegistry};

/// The assistant turn the run should act on.
pub(super) struct Reply {
    pub content: String,
    pub reasoning: String,
    pub calls: Vec<ToolCall>,
    pub prompt_tokens: Option<usize>,
}

pub(super) enum Next {
    Reply(Reply),
    /// History was force-compacted; send the smaller request right away.
    Retry,
}

pub(super) async fn request(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    cancel: &Cancel,
    ctx: &RunContext,
    overflow_retried: &mut bool,
) -> Result<Next, String> {
    match stream_step(ctx, client, config, tools, messages, cancel).await {
        Ok((content, reasoning, calls, prompt_tokens)) => {
            *overflow_retried = false;
            Ok(Next::Reply(Reply {
                content,
                reasoning,
                calls,
                prompt_tokens,
            }))
        }
        Err(err) if err.is_context_overflow() && !*overflow_retried => {
            *overflow_retried = true;
            match compaction::force(client, config, messages, tools, cancel).await {
                Ok(Some(compacted)) => {
                    note_compaction(
                        estimate_tokens(messages),
                        estimate_tokens(&compacted.messages),
                    );
                    ctx.record_archived(&compacted.replaced);
                    ensure_history_tool(tools, ctx);
                    *messages = compacted.messages;
                    Ok(Next::Retry)
                }
                Ok(None) => Err(format!(
                    "{err}\n\nThe conversation could not be compacted further; \
                     start a new chat or raise the context window."
                )),
                Err(force_err) => Err(force_err),
            }
        }
        Err(err) => Err(err.to_string()),
    }
}
