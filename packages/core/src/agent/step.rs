//! One model request, including the single context-overflow retry.
//!
//! A provider can reject a prompt that was under the local trigger; the only
//! recovery is to compact harder and retry once before reporting the error.

use serde_json::Value;

use super::model::{Usage, stream_step};
use super::{SystemPrompt, ToolCall};
use crate::AppConfig;
use crate::chat::RunContext;
use crate::tools::{Cancel, ToolRegistry};

/// Total attempts for a transient provider failure, including the first.
const MAX_ATTEMPTS: usize = 3;

/// The assistant turn the run should act on.
pub(super) struct Reply {
    pub content: String,
    pub reasoning: String,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

pub(super) enum Next {
    Reply(Reply),
    /// History was force-compacted; send the smaller request right away.
    Retry,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn request(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    prompt: &mut SystemPrompt,
    cancel: &Cancel,
    ctx: &RunContext,
    overflow_retried: &mut bool,
) -> Result<Next, String> {
    let mut attempt = 0;
    loop {
        match stream_step(ctx, client, config, tools, messages, cancel).await {
            Ok((content, reasoning, calls, usage)) => {
                *overflow_retried = false;
                return Ok(Next::Reply(Reply {
                    content,
                    reasoning,
                    calls,
                    usage,
                }));
            }
            Err(err) if err.is_transient() && attempt + 1 < MAX_ATTEMPTS => {
                let delay = err.retry_delay(attempt);
                attempt += 1;
                crate::log::key(format_args!(
                    "[oaklet] model retry {attempt} after {} ms: {err}",
                    delay.as_millis()
                ));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = cancel.cancelled() => return Err("Stopped.".into()),
                }
            }
            Err(err) if err.is_context_overflow() && !*overflow_retried => {
                *overflow_retried = true;
                match super::window::recover(client, config, tools, messages, prompt, cancel, ctx)
                    .await
                {
                    Ok(true) => return Ok(Next::Retry),
                    Ok(false) => {
                        return Err(format!(
                            "{err}\n\nThe conversation could not be compacted further; \
                             start a new chat or raise the context window."
                        ));
                    }
                    Err(force_err) => return Err(force_err),
                }
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}
