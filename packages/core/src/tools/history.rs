//! Read messages that compaction replaced.
//!
//! The archive is append-only, so a checkpoint can hide exact commands,
//! errors, and file contents without deleting them. This tool is registered
//! only when a chat has something archived, which keeps it out of the fixed
//! schema for short conversations.

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::Store;

use super::{Tool, ToolContext, ToolError, ToolOutput, parse_args};

const DEFAULT_PAGE: usize = 8;
const MAX_PAGE: usize = 50;
/// Keep one page under the pipeline's oversized-output threshold so the model
/// receives the whole page inline.
const PAGE_CHARS: usize = 10_000;

pub struct HistoryRead {
    store: Store,
    session_id: String,
    /// Compactions from the current run, not yet written by `finish_run`.
    pending: Arc<Mutex<Vec<Value>>>,
}

impl HistoryRead {
    pub fn new(store: Store, session_id: String, pending: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            store,
            session_id,
            pending,
        }
    }

    /// Whether the model should see this tool at all.
    pub fn has_entries(&self) -> bool {
        self.pending
            .lock()
            .map(|pending| !pending.is_empty())
            .unwrap_or(false)
            || self.store.history_count(&self.session_id).unwrap_or(0) > 0
    }

    fn page(&self, offset: usize, limit: usize) -> Result<String, String> {
        let stored = self.store.history_count(&self.session_id)?;
        let pending = self
            .pending
            .lock()
            .map_err(|_| "The archived history is unavailable.".to_string())?
            .clone();
        let total = stored + pending.len();
        if total == 0 {
            return Ok("No archived history in this chat.".into());
        }

        let offset = offset.max(1);
        if offset > total {
            return Ok(format!(
                "Archived history has {total} messages; offset {offset} is past the end."
            ));
        }
        let limit = limit.clamp(1, MAX_PAGE);

        let mut page: Vec<(usize, Value)> = Vec::new();
        if offset <= stored {
            let take = limit.min(stored - offset + 1);
            let (rows, _) = self.store.read_history(&self.session_id, offset, take)?;
            page.extend(
                rows.into_iter()
                    .map(|(seq, message)| (seq as usize, message)),
            );
        }
        if page.len() < limit {
            let next = offset + page.len();
            if next > stored {
                let start = next - stored - 1;
                for (index, message) in pending
                    .into_iter()
                    .skip(start)
                    .take(limit - page.len())
                    .enumerate()
                {
                    page.push((stored + start + index + 1, message));
                }
            }
        }
        if page.is_empty() {
            return Ok(format!("No archived messages at offset {offset}."));
        }

        let mut rendered = String::new();
        let mut shown: usize = 0;
        for (index, message) in &page {
            let block = render_message(*index, message);
            if shown > 0 && rendered.chars().count() + block.chars().count() + 2 > PAGE_CHARS {
                break;
            }
            rendered.push_str(&block);
            rendered.push_str("\n\n");
            shown += 1;
        }
        let end = offset + shown.saturating_sub(1);
        let mut output = format!("[archived history: messages {offset}-{end} of {total}]\n\n");
        output.push_str(rendered.trim_end_matches('\n'));
        if end < total {
            output.push_str(&format!(
                "\n\n[next: history_read(offset={}, limit={limit})]",
                end + 1
            ));
        }
        Ok(output)
    }
}

impl Tool for HistoryRead {
    fn name(&self) -> &str {
        "history_read"
    }

    fn description(&self) -> &str {
        "Read the original messages that compaction replaced in this chat. Use it when a checkpoint may have hidden an exact earlier command, error, or file snippet. Pages are archived messages, 1-based."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "offset": { "type": "integer", "description": "First archived message to read, 1-based. Defaults to 1." },
                "limit": { "type": "integer", "description": "Maximum number of messages. Defaults to 8." }
            },
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &str {
        "After compaction, exact earlier commands, errors, and file contents remain available through history_read; use it instead of guessing."
    }

    fn summary(&self, args: &Value) -> String {
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1);
        format!("from message {offset}")
    }

    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: HistoryReadArgs = parse_args(args)?;
            self.page(args.offset.unwrap_or(1), args.limit.unwrap_or(DEFAULT_PAGE))
                .map_err(ToolError::Failed)
                .map(ToolOutput::text)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryReadArgs {
    offset: Option<usize>,
    limit: Option<usize>,
}

fn render_message(index: usize, message: &Value) -> String {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut output = String::new();
    match role {
        "assistant" => {
            output.push_str(&format!("--- {index}. Assistant ---\n"));
            let content = content_text(message.get("content"));
            if !content.trim().is_empty() {
                output.push_str(&content);
                output.push('\n');
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                output.push_str("Tool calls:\n");
                for call in calls {
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let arguments = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                    output.push_str(&format!("- {name}({arguments})"));
                    if !id.is_empty() {
                        output.push_str(&format!(" [{id}]"));
                    }
                    output.push('\n');
                }
            }
        }
        "tool" => {
            let id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if id.is_empty() {
                output.push_str(&format!("--- {index}. Tool result ---\n"));
            } else {
                output.push_str(&format!("--- {index}. Tool result ({id}) ---\n"));
            }
            output.push_str(&content_text(message.get("content")));
        }
        other => {
            let label = match other {
                "user" => "User".to_string(),
                "system" => "System".to_string(),
                _ => other.to_string(),
            };
            output.push_str(&format!("--- {index}. {label} ---\n"));
            output.push_str(&content_text(message.get("content")));
        }
    }
    output.trim_end_matches('\n').to_string()
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    text.to_string()
                } else if part
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.contains("image"))
                {
                    "[image]".to_string()
                } else {
                    format!(
                        "[{}]",
                        part.get("type").and_then(Value::as_str).unwrap_or("part")
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use crate::store::{ContextUsage, Store};
    use crate::tools::test_support::context;

    use super::{HistoryRead, Tool, render_message};

    #[test]
    fn renders_tool_calls_and_results_verbatim() {
        let assistant = json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "run_command", "arguments": "{\"command\":\"cargo test\"}" }
            }]
        });
        let text = render_message(2, &assistant);
        assert!(text.contains("Assistant"));
        assert!(text.contains("run_command({\"command\":\"cargo test\"})"));
        assert!(text.contains("[call_1]"));

        let result = render_message(
            3,
            &json!({ "role": "tool", "tool_call_id": "call_1", "content": "FAILED at line 7" }),
        );
        assert!(result.contains("Tool result (call_1)"));
        assert!(result.contains("FAILED at line 7"));
    }

    #[test]
    fn renders_multimodal_user_content() {
        let message = json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "look" },
                { "type": "image_url", "image_url": { "url": "data:..." } }
            ]
        });
        let text = render_message(1, &message);
        assert!(text.contains("look"));
        assert!(text.contains("[image]"));
    }

    #[test]
    fn reports_entries_only_after_something_is_archived() {
        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        let tool = HistoryRead::new(store.clone(), "s1".into(), Arc::new(Mutex::new(Vec::new())));
        assert!(!tool.has_entries());

        store
            .apply_compaction(
                "s1",
                &[json!({ "role": "user", "content": "summary" })],
                &[json!({ "role": "user", "content": "old" })],
                &json!({ "kind": "notice" }),
                ContextUsage {
                    used: 1,
                    window: 10,
                },
                2,
            )
            .unwrap();
        assert!(tool.has_entries());
    }

    #[tokio::test]
    async fn pages_stored_and_pending_archives() {
        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        store
            .apply_compaction(
                "s1",
                &[json!({ "role": "user", "content": "summary" })],
                &[
                    json!({ "role": "user", "content": "old request" }),
                    json!({ "role": "assistant", "content": "old answer" }),
                ],
                &json!({ "kind": "notice" }),
                ContextUsage {
                    used: 1,
                    window: 10,
                },
                2,
            )
            .unwrap();

        let (_ctx, root, _handle) = context("history-tool");
        let tool = HistoryRead::new(
            store,
            "s1".into(),
            Arc::new(Mutex::new(vec![json!({
                "role": "tool",
                "tool_call_id": "call_2",
                "content": "fresh but uncommitted"
            })])),
        );

        let first = tool.page(1, 2).unwrap();
        assert!(first.contains("old request"));
        assert!(first.contains("old answer"));
        assert!(first.contains("offset=3"));

        let second = tool.page(3, 2).unwrap();
        assert!(second.contains("fresh but uncommitted"));
        assert!(second.contains("of 3"));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn executes_with_one_based_paging_arguments() {
        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        store
            .apply_compaction(
                "s1",
                &[json!({ "role": "user", "content": "summary" })],
                &[
                    json!({ "role": "user", "content": "first" }),
                    json!({ "role": "user", "content": "second" }),
                ],
                &json!({ "kind": "notice" }),
                ContextUsage {
                    used: 1,
                    window: 10,
                },
                2,
            )
            .unwrap();

        let (ctx, root, _handle) = context("history-tool-args");
        let tool = HistoryRead::new(store, "s1".into(), Arc::new(Mutex::new(Vec::new())));
        let page = tool
            .execute(&ctx, json!({ "offset": 2, "limit": 1 }))
            .await
            .unwrap();
        assert!(page.contains("second"));
        assert!(!page.contains("first"));
        std::fs::remove_dir_all(root).ok();
    }
}
