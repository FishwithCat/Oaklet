//! Update the live transcript and emit window-scoped streaming events.
use serde::Serialize;
use serde_json::{Value, json};

use super::RunContext;
use crate::chat::transcript::{
    NewItem, ToolItem, ToolStatus, find_tool_item, merge_stream, refresh_blocked, settle_streaming,
};
use crate::context;
use crate::store::{self, ContextUsage, EventPayload};
use crate::tools::{FileDiff, ProcessOutcome};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaEvent {
    session_id: String,
    run_id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningEvent {
    session_id: String,
    run_id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolStartEvent {
    session_id: String,
    run_id: String,
    id: String,
    name: String,
    summary: String,
    diffs: Vec<FileDiff>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultEvent {
    session_id: String,
    run_id: String,
    id: String,
    ok: bool,
    output: String,
    diffs: Vec<FileDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<ProcessOutcome>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalEvent {
    session_id: String,
    run_id: String,
    id: String,
    request_id: String,
    tool: String,
    args: Value,
    prompt: String,
    detail: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalResolvedEvent {
    session_id: String,
    run_id: String,
    id: String,
    allow: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextEvent {
    session_id: String,
    run_id: String,
    #[serde(flatten)]
    context: ContextUsage,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageEvent {
    session_id: String,
    run_id: String,
    #[serde(flatten)]
    usage: store::RunUsage,
}

impl RunContext {
    pub fn delta(&self, text: &str) {
        {
            let mut partial = self.partial.lock().unwrap_or_else(|p| p.into_inner());
            partial.settle_reasoning(store::now_ms() as u64);
            partial.content.push_str(text);
        }
        self.update_view(|view| {
            merge_stream(&mut view.items, &self.run_id, "assistant", text);
            view.active_run_id = Some(self.run_id.clone());
        });
        self.emit(
            "agent:delta",
            DeltaEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                text: text.to_string(),
            },
        );
    }

    pub fn reasoning(&self, text: &str) {
        {
            let mut partial = self.partial.lock().unwrap_or_else(|p| p.into_inner());
            partial
                .reasoning_started_at
                .get_or_insert_with(|| store::now_ms() as u64);
            partial.reasoning.push_str(text);
        }
        self.update_view(|view| {
            merge_stream(&mut view.items, &self.run_id, "reasoning", text);
            view.active_run_id = Some(self.run_id.clone());
        });
        self.emit(
            "agent:reasoning",
            ReasoningEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                text: text.to_string(),
            },
        );
    }

    pub fn tool_start(&self, id: &str, name: &str, summary: &str, diffs: &[FileDiff]) {
        let started_at = store::now_ms() as u64;
        self.record(EventPayload::Message {
            message: json!({ "role": "tool", "tool_call_id": id, "content": "" }),
            display: Some(json!({
                "runId": self.run_id,
                "name": name,
                "summary": summary,
                "ok": false,
                "pending": true,
                "diffs": diffs,
            })),
        });
        self.update_view(|view| {
            settle_streaming(&mut view.items);
            let item = NewItem::Tool(Box::new(ToolItem {
                run_id: self.run_id.clone(),
                id: id.to_string(),
                name: name.to_string(),
                status: ToolStatus::Pending,
                summary: summary.to_string(),
                output: None,
                started_at: Some(started_at),
                duration_ms: None,
                request_id: None,
                approval_prompt: None,
                approval_detail: None,
                diffs: diffs.to_vec(),
                outcome: None,
            }));
            match serde_json::to_value(item) {
                Ok(item) => view.items.push(item),
                Err(error) => crate::log::key(format_args!(
                    "[oaklet] could not record a tool call: {error}"
                )),
            }
            view.active_run_id = Some(self.run_id.clone());
            refresh_blocked(view);
        });
        self.emit(
            "agent:tool_start",
            ToolStartEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                id: id.to_string(),
                name: name.to_string(),
                summary: summary.to_string(),
                diffs: diffs.to_vec(),
            },
        );
    }

    pub fn tool_result(
        &self,
        id: &str,
        ok: bool,
        output: &str,
        diffs: &[FileDiff],
        outcome: Option<&ProcessOutcome>,
    ) -> Value {
        let finished_at = store::now_ms() as u64;
        let mut duration_ms = None;
        self.update_view(|view| {
            if let Some(item) = find_tool_item(&mut view.items, id) {
                let status = if ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Error
                };
                item["status"] = json!(status);
                item["output"] = json!(output);
                if diffs.is_empty() {
                    if let Some(object) = item.as_object_mut() {
                        object.remove("diffs");
                    }
                } else {
                    item["diffs"] = json!(diffs);
                }
                if let Some(outcome) = outcome {
                    item["outcome"] = json!(outcome);
                }
                if let Some(started_at) = item.get("startedAt").and_then(Value::as_u64) {
                    duration_ms = Some(finished_at.saturating_sub(started_at));
                    item["durationMs"] = json!(duration_ms);
                }
                if let Some(object) = item.as_object_mut() {
                    object.remove("startedAt");
                }
            }
            refresh_blocked(view);
        });
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut found = false;
            for payload in pending.iter_mut().rev() {
                let EventPayload::Message { message, display } = payload else {
                    continue;
                };
                let pending_tool = message.get("tool_call_id").and_then(Value::as_str) == Some(id)
                    && display
                        .as_ref()
                        .and_then(|display| display.get("pending"))
                        .and_then(Value::as_bool)
                        == Some(true);
                if !pending_tool {
                    continue;
                }
                message["content"] = json!(output);
                if let Some(meta) = display.as_mut().and_then(Value::as_object_mut) {
                    meta["ok"] = json!(ok);
                    if let Some(duration) = duration_ms {
                        meta.insert("durationMs".into(), json!(duration));
                    }
                    meta.remove("pending");
                    if diffs.is_empty() {
                        meta.remove("diffs");
                    } else {
                        meta["diffs"] = json!(diffs);
                    }
                    if let Some(outcome) = outcome {
                        meta.insert("outcome".into(), json!(outcome));
                    }
                }
                found = true;
                break;
            }
            if !found {
                drop(pending);
                let mut display = json!({
                    "runId": self.run_id,
                    "ok": ok,
                    "diffs": diffs,
                });
                if let Some(outcome) = outcome {
                    display["outcome"] = json!(outcome);
                }
                self.record(EventPayload::Message {
                    message: json!({ "role": "tool", "tool_call_id": id, "content": output }),
                    display: Some(display),
                });
            }
        }
        self.emit(
            "agent:tool_result",
            ToolResultEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                id: id.to_string(),
                ok,
                output: output.to_string(),
                diffs: diffs.to_vec(),
                outcome: outcome.cloned(),
            },
        );
        let pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let (index, message) = pending
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, event)| match event {
                EventPayload::Message { message, .. }
                    if message["role"] == "tool" && message["tool_call_id"] == id =>
                {
                    Some((index, message))
                }
                _ => None,
            })
            .expect("tool_result records its message before publishing");
        context::identify(message.clone(), context::MessageId::Pending(index))
    }

    pub fn approval(
        &self,
        id: &str,
        request_id: &str,
        tool: &str,
        args: Value,
        prompt: &str,
        detail: &str,
    ) {
        self.update_view(|view| {
            if let Some(item) = find_tool_item(&mut view.items, id) {
                item["status"] = json!(ToolStatus::Awaiting);
                item["requestId"] = json!(request_id);
                item["approvalPrompt"] = json!(prompt);
                item["approvalDetail"] = json!(detail);
            }
            refresh_blocked(view);
        });
        self.emit(
            "agent:approval",
            ApprovalEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                id: id.to_string(),
                request_id: request_id.to_string(),
                tool: tool.to_string(),
                args,
                prompt: prompt.to_string(),
                detail: detail.to_string(),
            },
        );
    }

    pub fn approval_resolved(&self, id: &str, allow: bool) {
        self.update_view(|view| {
            if let Some(item) = find_tool_item(&mut view.items, id) {
                item["status"] = json!(ToolStatus::Running);
                if allow {
                    item["startedAt"] = json!(store::now_ms() as u64);
                } else if let Some(object) = item.as_object_mut() {
                    object.remove("startedAt");
                }
                if let Some(object) = item.as_object_mut() {
                    object.remove("requestId");
                    object.remove("approvalPrompt");
                    object.remove("approvalDetail");
                }
            }
            refresh_blocked(view);
        });
        self.emit(
            "agent:approval_resolved",
            ApprovalResolvedEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                id: id.to_string(),
                allow,
            },
        );
    }

    pub fn context(&self, usage: ContextUsage) {
        self.update_view(|view| view.context = Some(usage.clone()));
        self.emit(
            "agent:context",
            ContextEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                context: usage,
            },
        );
    }

    pub(crate) fn emit_usage(&self, usage: store::RunUsage) {
        self.emit(
            "agent:usage",
            UsageEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                usage,
            },
        );
    }
}
