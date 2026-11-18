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
/// Pack small messages inline; a single oversized message uses the pipeline's recoverable spill file.
const PAGE_CHARS: usize = 10_000;

#[derive(Clone)]
pub struct HistoryRead {
    store: Store,
    session_id: String,
    /// Compactions from the current run, not yet written by `finish_run`.
    pending: Arc<Mutex<Vec<(i64, Value)>>>,
}

impl HistoryRead {
    pub fn new(store: Store, session_id: String, pending: Arc<Mutex<Vec<(i64, Value)>>>) -> Self {
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
            || self.store.has_history(&self.session_id).unwrap_or(false)
    }

    fn image(&self, offset: usize, image: usize) -> Result<ToolOutput, String> {
        if image == 0 {
            return Err("Image numbers start at 1.".into());
        }
        let pending = self
            .pending
            .lock()
            .map_err(|_| "The archive is unavailable.")?;
        let (rows, _) =
            self.store
                .read_history_pending(&self.session_id, offset.max(1), 1, &pending)?;
        let message = rows
            .first()
            .map(|(_, message)| message)
            .ok_or("No archived message at that offset.")?;
        let url = message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| matches!(part["type"].as_str(), Some("image_url" | "input_image")))
            .nth(image - 1)
            .and_then(|part| {
                part.pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .or_else(|| part["image_url"].as_str())
                    .or_else(|| part["url"].as_str())
            })
            .ok_or("No image with that number in the archived message.")?;
        Ok(ToolOutput::with_image(
            format!(
                "Original image {image} from archived message {}.",
                offset.max(1)
            ),
            url.to_string(),
        ))
    }

    fn page(&self, offset: usize, limit: usize) -> Result<String, String> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| "The archived history is unavailable.".to_string())?;
        let offset = offset.max(1);
        let limit = limit.clamp(1, MAX_PAGE);
        let (rows, total) =
            self.store
                .read_history_pending(&self.session_id, offset, limit, &pending)?;
        if total == 0 {
            return Ok("No archived history in this chat.".into());
        }

        if offset > total {
            return Ok(format!(
                "Archived history has {total} messages; offset {offset} is past the end."
            ));
        }
        let page: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, (_, message))| (offset + index, message))
            .collect();
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
        "Read the original messages that compaction replaced in this chat. Use it when a checkpoint may have hidden an exact earlier command, error, or file snippet. Pages are archived messages, 1-based. To inspect an original image, use offset and image from its locator."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "offset": { "type": "integer", "description": "First archived message to read, 1-based. Defaults to 1." },
                "limit": { "type": "integer", "description": "Maximum number of messages. Defaults to 8." },
                "image": { "type": "integer", "minimum": 1, "description": "Attach this 1-based original image from the message at offset. Omit to read text." }
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
            let tool = self.clone();
            tauri::async_runtime::spawn_blocking(move || match args.image {
                Some(image) => tool.image(args.offset.unwrap_or(1), image),
                None => tool
                    .page(args.offset.unwrap_or(1), args.limit.unwrap_or(DEFAULT_PAGE))
                    .map(ToolOutput::text),
            })
            .await
            .map_err(|error| {
                ToolError::Failed(format!("Could not read archived history: {error}"))
            })?
            .map_err(ToolError::Failed)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryReadArgs {
    offset: Option<usize>,
    limit: Option<usize>,
    image: Option<usize>,
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
    let images = message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| matches!(part["type"].as_str(), Some("image_url" | "input_image")))
        .count();
    for image in 1..=images {
        output.push_str(&format!(
            "\n[original image {image}: history_read(offset={index}, image={image})]"
        ));
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

    use crate::store::{ContextUsage, EventPayload, RunEnd, Store};
    use crate::tools::test_support::context;

    use super::{HistoryRead, Tool, render_message};

    fn archive(store: &Store, session: &str, messages: Vec<serde_json::Value>) {
        let shadowed_through = messages.len() as i64;
        store
            .begin_run(
                session,
                "r1",
                &crate::store::NewUserTurn {
                    message: messages[0].clone(),
                    display: None,
                    title: None,
                },
                1,
            )
            .unwrap();
        store
            .finish_run(&RunEnd {
                session_id: session.into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 2,
                appended: messages
                    .into_iter()
                    .skip(1)
                    .map(|message| EventPayload::Message {
                        message,
                        display: None,
                    })
                    .collect(),
                compaction: None,
                context: Some(ContextUsage {
                    used: 1,
                    window: 10,
                }),
                error: None,
            })
            .unwrap();
        store
            .apply_compaction(
                session,
                &crate::store::CompactionRecord {
                    summary: json!({
                        "role": "user",
                        "content": "<compacted-summary>earlier turns</compacted-summary>",
                    }),
                    shadowed_through,
                    before: 1,
                    after: 1,
                    context: ContextUsage {
                        used: 1,
                        window: 10,
                    },
                },
                2,
            )
            .unwrap();
    }

    #[test]
    fn archive_numbers_follow_offsets_across_event_gaps() {
        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        archive(
            &store,
            "s1",
            vec![
                json!({"role":"user", "content":"first"}),
                json!({"role":"assistant", "content":"second"}),
            ],
        );
        let start = store
            .begin_run(
                "s1",
                "r2",
                &crate::store::NewUserTurn {
                    message: json!({"role":"user", "content":"third"}),
                    display: None,
                    title: None,
                },
                3,
            )
            .unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r2".into(),
                start_seq: start.start_seq,
                status: "done",
                finished_at: 4,
                appended: vec![
                    EventPayload::Notice {
                        tone: "muted".into(),
                        text: "gap".into(),
                    },
                    EventPayload::Message {
                        message: json!({"role":"assistant", "content":"fourth"}),
                        display: None,
                    },
                ],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        store.apply_compaction("s1", &crate::store::CompactionRecord {
            summary:json!({"role":"user", "content":"<compacted-summary>all</compacted-summary>"}),
            shadowed_through:start.start_seq + 2, before:10, after:1, context:ContextUsage {used:1, window:10},
        }, 5).unwrap();
        let tool = HistoryRead::new(
            store,
            "s1".into(),
            Arc::new(Mutex::new(vec![(
                100,
                json!({"role":"user", "content":"pending"}),
            )])),
        );
        let page = tool.page(3, 2).unwrap();
        assert!(page.contains("--- 3. User ---\nthird"));
        assert!(page.contains("--- 4. Assistant ---\nfourth"));
        assert!(page.contains("offset=5"));
        assert!(
            tool.page(5, 1)
                .unwrap()
                .contains("--- 5. User ---\npending")
        );
        assert!(
            tool.page(4, 1)
                .unwrap()
                .contains("--- 4. Assistant ---\nfourth")
        );
    }

    #[tokio::test]
    async fn reattaches_the_original_archived_image() {
        let store = Store::in_memory().unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        let url = "data:image/png;base64,iVBORw0KGgo=";
        archive(
            &store,
            "s",
            vec![json!({"role":"user","content":[
                {"type":"text","text":"match this screenshot"},
                {"type":"image_url","image_url":{"url":url}}
            ]})],
        );
        let tool = HistoryRead::new(store, "s".into(), Arc::new(Mutex::new(Vec::new())));
        assert!(
            tool.page(1, 1)
                .unwrap()
                .contains("history_read(offset=1, image=1)")
        );
        let (ctx, root, _handle) = context("history-image");
        let output = tool
            .execute(&ctx, json!({"offset":1,"image":1}))
            .await
            .unwrap();
        assert_eq!(output.images[0].data_url, url);
        assert!(
            tool.execute(&ctx, json!({"offset":1,"image":0}))
                .await
                .is_err()
        );
        assert!(
            tool.execute(&ctx, json!({"offset":1,"image":2}))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

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

        archive(
            &store,
            "s1",
            vec![json!({ "role": "user", "content": "old" })],
        );
        assert!(tool.has_entries());
    }

    #[tokio::test]
    async fn pages_stored_and_pending_archives() {
        let store = Store::in_memory().unwrap();
        store.create_session("s1", "/tmp", 1).unwrap();
        archive(
            &store,
            "s1",
            vec![
                json!({ "role": "user", "content": "old request" }),
                json!({ "role": "assistant", "content": "old answer" }),
            ],
        );

        let (_ctx, root, _handle) = context("history-tool");
        let tool = HistoryRead::new(
            store,
            "s1".into(),
            Arc::new(Mutex::new(vec![(
                100,
                json!({
                    "role": "tool",
                    "tool_call_id": "call_2",
                    "content": "fresh but uncommitted"
                }),
            )])),
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
        archive(
            &store,
            "s1",
            vec![
                json!({ "role": "user", "content": "first" }),
                json!({ "role": "assistant", "content": "second" }),
            ],
        );

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

#[cfg(test)]
mod oversized_history_tests {
    use super::*;

    #[test]
    fn oversized_single_message_remains_recoverable_through_the_spill_pipeline() {
        let store = Store::in_memory().unwrap();
        store.create_session("s", "/tmp", 1).unwrap();
        let full = format!("HEAD{}MIDDLE{}TAIL", "x".repeat(20000), "y".repeat(20000));
        let tool = HistoryRead::new(
            store,
            "s".into(),
            Arc::new(Mutex::new(vec![(
                1,
                json!({"role":"user", "content":full}),
            )])),
        );
        let page = tool.page(1, 1).unwrap();
        let (ctx, root, _handle) = crate::tools::test_support::context("history-spill");
        let output = ctx.bound_output("history_read", "call-history", page.clone());
        let path = crate::tools::spill_pointer(&output).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), page);
        assert!(output.contains("Use read_file with offset and limit"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
