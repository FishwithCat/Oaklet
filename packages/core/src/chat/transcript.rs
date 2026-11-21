//! Rendered transcript projection and live row updates.
use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Value, json};

use super::SessionView;
use crate::store::{self, Event, EventPayload, Session};
use crate::tools::FileDiff;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ToolStatus {
    Pending,
    Awaiting,
    Running,
    Ok,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolItem {
    pub(super) run_id: String,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) status: ToolStatus,
    pub(super) summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) approval_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) approval_detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) diffs: Vec<FileDiff>,
}

/// A transcript row the backend creates. Streaming assistant/reasoning rows are
/// built directly as JSON so they can be merged in place.
#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum NewItem {
    Tool(Box<ToolItem>),
}

pub(super) fn view_from_session(session: Session, compacting: bool) -> SessionView {
    let first_seq = session.events.first().map(|event| event.seq);
    SessionView {
        id: session.id,
        workspace: session.workspace,
        title: session.title,
        updated_at: session.updated_at,
        turns: session.turns,
        items: project_events(&session.events),
        first_seq,
        has_more: first_seq.is_some_and(|seq| seq > 1),
        context: session.context,
        last_usage: None,
        running: false,
        blocked: false,
        compacting,
        active_run_id: None,
    }
}

pub(super) fn view_from_window(window: store::SessionWindow, compacting: bool) -> SessionView {
    let first_seq = window.events.first().map(|event| event.seq);
    SessionView {
        id: window.id,
        workspace: window.workspace,
        title: window.title,
        updated_at: window.updated_at,
        turns: window.turns,
        items: project_events(&window.events),
        first_seq,
        has_more: window.has_more,
        context: window.context,
        last_usage: window.last_usage,
        running: false,
        blocked: false,
        compacting,
        active_run_id: None,
    }
}

/// Text a user bubble shows; attachment bodies in the model message stay out
/// of the bubble.
fn item_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn display_flag(display: Option<&Value>, key: &str) -> bool {
    display
        .and_then(|display| display.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn display_str<'a>(display: Option<&'a Value>, key: &str) -> &'a str {
    display
        .and_then(|display| display.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The rendered transcript is a pure function of the event log. The live run
/// mutates the same shape only while tokens stream; the saved view replaces it
/// with this projection when the run ends.
fn project_events(events: &[Event]) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    // One card per version, rendered where the version first appeared and
    // carrying its newest status. A later accepted marker repeats the payload
    // so the card keeps its place above the confirmation turn.
    let mut plan_events: HashMap<u64, (i64, store::PlanStatus)> = HashMap::new();
    let mut latest_plan = 0;
    for event in events {
        if let EventPayload::Plan {
            version, status, ..
        } = event.payload
        {
            plan_events
                .entry(version)
                .and_modify(|(_, latest)| *latest = status)
                .or_insert((event.seq, status));
            latest_plan = latest_plan.max(version);
        }
    }

    for event in events {
        match &event.payload {
            EventPayload::ToolTrim { .. } => {}
            EventPayload::Plan {
                version,
                steps,
                validation,
                ..
            } => {
                let Some((first_seq, latest_status)) = plan_events.get(version) else {
                    continue;
                };
                if *first_seq != event.seq {
                    continue;
                }
                // The card renders the same final text, so keep it from being
                // shown twice in the transcript.
                if let Some(last) = items.last()
                    && last.get("kind").and_then(Value::as_str) == Some("assistant")
                    && last.get("text").and_then(Value::as_str) == Some(steps.as_str())
                {
                    items.pop();
                }
                items.push(json!({
                    "kind": "plan",
                    "seq": event.seq,
                    "version": version,
                    "status": latest_status,
                    "latest": *version == latest_plan,
                    "steps": steps,
                    "validation": validation,
                }));
            }
            EventPayload::Notice { tone, text } => items.push(json!({
                "kind": "notice",
                "seq": event.seq,
                "tone": tone,
                "text": text,
            })),
            EventPayload::Compaction { before, after, .. }
            | EventPayload::Context { before, after, .. } => items.push(json!({
                "kind": "notice",
                "seq": event.seq,
                "tone": "muted",
                "text": format!(
                    "Compacted context: {} → {} tokens.",
                    format_tokens(*before),
                    format_tokens(*after)
                ),
            })),
            EventPayload::Message { message, display } => {
                if display_flag(display.as_ref(), "hidden") {
                    continue;
                }
                let run_id = display_str(display.as_ref(), "runId");
                match message.get("role").and_then(Value::as_str).unwrap_or("") {
                    "user" => {
                        if store::is_checkpoint(message) {
                            continue;
                        }
                        let text = display
                            .as_ref()
                            .and_then(|display| display.get("text"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| item_text(message.get("content")));
                        let mut item = json!({ "kind": "user", "text": text });
                        item["seq"] = json!(event.seq);
                        if let Some(attachments) = display
                            .as_ref()
                            .and_then(|display| display.get("attachments"))
                            .filter(|value| value.as_array().is_some_and(|list| !list.is_empty()))
                        {
                            item["attachments"] = attachments.clone();
                        }
                        items.push(item);
                    }
                    "assistant" => {
                        let reasoning = message
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if !reasoning.is_empty() {
                            let mut item = json!({
                                "kind": "reasoning",
                                "seq": event.seq,
                                "runId": run_id,
                                "text": reasoning,
                                "streaming": false,
                            });
                            if let Some(duration) = display
                                .as_ref()
                                .and_then(|display| display.get("reasoningDurationMs"))
                                .and_then(Value::as_u64)
                            {
                                item["durationMs"] = json!(duration);
                            }
                            items.push(item);
                        }
                        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                            for call in calls {
                                let id = call.get("id").and_then(Value::as_str);
                                let name = call.pointer("/function/name").and_then(Value::as_str);
                                if let (Some(id), Some(name)) = (id, name) {
                                    tool_names.insert(id.to_string(), name.to_string());
                                }
                            }
                        }
                        let content = item_text(message.get("content"));
                        if !content.is_empty() {
                            items.push(json!({
                                "kind": "assistant",
                                "seq": event.seq,
                                "runId": run_id,
                                "text": content,
                                "streaming": false,
                            }));
                        }
                    }
                    "tool" => {
                        let id = message
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let name = display
                            .as_ref()
                            .and_then(|display| display.get("name"))
                            .and_then(Value::as_str)
                            .or_else(|| tool_names.get(id).map(String::as_str))
                            .unwrap_or("");
                        let summary = display_str(display.as_ref(), "summary");
                        let ok = display
                            .as_ref()
                            .and_then(|display| display.get("ok"))
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        let pending = display_flag(display.as_ref(), "pending");
                        let mut item = json!({
                            "kind": "tool",
                            "seq": event.seq,
                            "runId": run_id,
                            "id": id,
                            "name": name,
                            "status": if ok && !pending { "ok" } else { "error" },
                            "summary": summary,
                            "output": message.get("content").and_then(Value::as_str).unwrap_or(""),
                        });
                        if let Some(duration) = display
                            .as_ref()
                            .and_then(|display| display.get("durationMs"))
                            .and_then(Value::as_u64)
                        {
                            item["durationMs"] = json!(duration);
                        }
                        if let Some(diffs) = display
                            .as_ref()
                            .and_then(|display| display.get("diffs"))
                            .and_then(Value::as_array)
                            .filter(|diffs| !diffs.is_empty() && !pending)
                        {
                            item["diffs"] = Value::Array(diffs.clone());
                        }
                        items.push(item);
                    }
                    _ => {}
                }
            }
        }
    }
    items
}

fn format_tokens(tokens: usize) -> String {
    if tokens < 1024 {
        return tokens.to_string();
    }
    let thousands = tokens as f64 / 1024.0;
    if thousands < 10.0 {
        format!("{thousands:.1}k")
    } else {
        format!("{}k", thousands.round() as usize)
    }
}

pub(super) fn merge_stream(items: &mut Vec<Value>, run_id: &str, kind: &str, text: &str) {
    let can_merge = items
        .last()
        .map(|last| {
            last.get("kind").and_then(Value::as_str) == Some(kind)
                && last.get("streaming").and_then(Value::as_bool) == Some(true)
                && (kind == "assistant"
                    || last.get("runId").and_then(Value::as_str) == Some(run_id))
        })
        .unwrap_or(false);

    if can_merge {
        let last = items.last_mut().expect("checked above");
        let combined = format!(
            "{}{}",
            last.get("text").and_then(Value::as_str).unwrap_or(""),
            text
        );
        last["text"] = Value::String(combined);
        return;
    }

    settle_streaming(items);
    items.push(json!({
        "kind": kind,
        "runId": run_id,
        "text": text,
        "streaming": true,
    }));
    if kind == "reasoning" {
        items.last_mut().expect("just pushed")["startedAt"] = json!(store::now_ms() as u64);
    }
}

pub(super) fn settle_streaming(items: &mut [Value]) {
    for item in items.iter_mut() {
        let kind = item.get("kind").and_then(Value::as_str);
        let reasoning = kind == Some("reasoning");
        if (kind == Some("assistant") || kind == Some("reasoning"))
            && item.get("streaming").and_then(Value::as_bool) == Some(true)
        {
            item["streaming"] = json!(false);
            if reasoning {
                if let Some(started_at) = item.get("startedAt").and_then(Value::as_u64) {
                    item["durationMs"] = json!((store::now_ms() as u64).saturating_sub(started_at));
                }
                if let Some(object) = item.as_object_mut() {
                    object.remove("startedAt");
                }
            }
        }
    }
}

pub(super) fn settle_unfinished_tools(items: &mut [Value]) {
    for item in items.iter_mut() {
        if item.get("kind").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        if matches!(status, "pending" | "awaiting" | "running") {
            item["status"] = json!(ToolStatus::Error);
            if let Some(object) = item.as_object_mut() {
                object.remove("requestId");
                object.remove("startedAt");
                object.remove("approvalPrompt");
                object.remove("approvalDetail");
                object.remove("diffs");
            }
        }
    }
}

fn is_blocked(items: &[Value]) -> bool {
    items.iter().any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item.get("status").and_then(Value::as_str) == Some("awaiting")
    })
}

pub(super) fn refresh_blocked(view: &mut SessionView) {
    view.blocked = is_blocked(&view.items);
}

pub(super) fn find_tool_item<'a>(items: &'a mut [Value], id: &str) -> Option<&'a mut Value> {
    items.iter_mut().rev().find(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item.get("id").and_then(Value::as_str) == Some(id)
    })
}

#[cfg(test)]
mod tests;
