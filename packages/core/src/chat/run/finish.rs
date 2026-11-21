//! Settle interrupted work, save the turn and publish its terminal state.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::Serialize;
use serde_json::{Value, json};

use super::{RunContext, trim_incomplete_tool_block};
use crate::chat::transcript::{settle_streaming, settle_unfinished_tools};
use crate::chat::{SessionView, Tone, notices::resume_notices};
use crate::store::{self, EventPayload, RunEnd};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    session_id: String,
    run_id: String,
    session: Option<SessionView>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    session_id: String,
    run_id: String,
    message: String,
    session: Option<SessionView>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoppedEvent {
    session_id: String,
    run_id: String,
    session: Option<SessionView>,
}

/// Give every unanswered assistant tool call a synthetic error result in the
/// run's pending events, preserving call order and existing started markers.
fn close_incomplete_tool_calls(pending: &mut Vec<EventPayload>, run_id: &str) {
    // Existing tool messages are created by `tool_start` and updated in place
    // by `tool_result`; remember whether each call has an outcome.
    let mut tool_message: HashMap<String, (usize, bool)> = HashMap::new();
    for (index, payload) in pending.iter().enumerate() {
        let EventPayload::Message { message, display } = payload else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(id) = message.get("tool_call_id").and_then(Value::as_str) else {
            continue;
        };
        let waiting = display
            .as_ref()
            .and_then(|display| display.get("pending"))
            .and_then(Value::as_bool)
            == Some(true);
        tool_message.insert(id.to_string(), (index, waiting));
    }

    let mut missing: Vec<(String, String)> = Vec::new();
    for payload in pending.iter() {
        let EventPayload::Message { message, .. } = payload else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let Some(id) = call.get("id").and_then(Value::as_str) else {
                continue;
            };
            if tool_message.get(id).is_some_and(|(_, waiting)| !*waiting) {
                continue;
            }
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            missing.push((id.to_string(), name));
        }
    }

    for (id, name) in missing {
        let started = tool_message.get(&id).map(|(index, _)| *index);
        let text = if started.is_some() {
            "The tool call was interrupted after it started, but no result was recorded. \
             Its outcome is unknown; verify external state before retrying."
        } else {
            "The tool call was interrupted before it started. Retry it if it is still needed."
        };
        if let Some(index) = started {
            if let EventPayload::Message { message, display } = &mut pending[index] {
                message["content"] = json!(text);
                if let Some(display) = display.as_mut().and_then(Value::as_object_mut) {
                    display.insert("ok".into(), json!(false));
                    display.remove("pending");
                }
            }
        } else {
            pending.push(EventPayload::Message {
                message: json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": text,
                }),
                display: Some(json!({
                    "runId": run_id,
                    "name": name,
                    "ok": false,
                    "hidden": true,
                })),
            });
        }
    }
}

impl RunContext {
    /// Give every unanswered assistant tool call a durable error result.
    ///
    /// A stop or provider failure can land between the assistant message and
    /// its tool results. Persisting that gap would make the next request
    /// invalid, so close it here while the run still owns its event buffer.
    fn repair_incomplete_tool_calls(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        close_incomplete_tool_calls(&mut pending, &self.run_id);
    }

    pub async fn finish_done(&self, messages: Vec<Value>) {
        self.finish(messages, "done", None).await;
    }

    pub async fn finish_error(&self, messages: Vec<Value>, message: impl Into<String>) {
        self.finish(messages, "error", Some(message.into())).await;
    }

    pub async fn finish_stopped(&self, messages: Vec<Value>) {
        self.finish(messages, "stopped", None).await;
    }

    /// Persist the whole turn, release the live handle, then publish the final
    /// view. If the write fails the run still ends, and the in-memory view is
    /// sent so the client keeps what it saw.
    async fn finish(&self, mut messages: Vec<Value>, status: &'static str, error: Option<String>) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if status == "done" {
            self.record_plan();
        }

        // A stopped or failed stream never reaches the assistant-message
        // handoff. Persist what the user already saw instead of letting the
        // database-backed final view erase the partial answer.
        let mut partial = std::mem::take(
            &mut *self
                .partial
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if status != "done" && (!partial.content.is_empty() || !partial.reasoning.is_empty()) {
            partial.settle_reasoning(store::now_ms() as u64);
            let mut display = json!({ "runId": self.run_id, "interrupted": true });
            if let Some(duration) = partial.reasoning_duration_ms {
                display["reasoningDurationMs"] = json!(duration);
            }
            let mut message = json!({ "role": "assistant", "content": partial.content });
            if !partial.reasoning.is_empty() {
                message["reasoning_content"] = json!(partial.reasoning);
            }
            messages.push(self.record_message(message, Some(display)));
        }

        let messages = trim_incomplete_tool_block(messages);

        let final_view = {
            let mut view = self
                .view
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.repair_incomplete_tool_calls();
            settle_streaming(&mut view.items);
            settle_unfinished_tools(&mut view.items);
            match status {
                "error" => {
                    let text = error.clone().unwrap_or_else(|| "The run failed.".into());
                    self.record_notice(Tone::Error, text.clone());
                    view.items
                        .push(json!({ "kind": "notice", "tone": Tone::Error, "text": text }));
                }
                "stopped" => {
                    self.record_notice(Tone::Muted, "Stopped.");
                    view.items
                        .push(json!({ "kind": "notice", "tone": Tone::Muted, "text": "Stopped." }));
                }
                _ => {}
            }
            view.running = false;
            view.active_run_id = None;
            view.blocked = false;
            view.clone()
        };

        let sizes = *self
            .compaction_sizes
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let compaction = self
            .compacted
            .load(Ordering::SeqCst)
            .then(|| store::CompactionEnd {
                surface: messages.clone(),
                before: sizes.map(|(before, _)| before).unwrap_or(0),
                after: sizes.map(|(_, after)| after).unwrap_or(0),
            });
        let end = Arc::new(RunEnd {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            start_seq: self.start_seq,
            status,
            finished_at: store::now_ms(),
            appended: std::mem::take(
                &mut *self
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
            compaction,
            context: final_view.context,
            usage: self.usage(),
            error: error.clone(),
        });
        let store = self.state.chat.store.clone();
        let saving = end.clone();
        let saved = tauri::async_runtime::spawn_blocking(move || store.finish_run(&saving)).await;

        let (session, save_error) = match saved {
            Ok(Ok(session)) => (Some(self.state.chat.view(session)), None),
            Ok(Err(error)) => (None, Some(error)),
            Err(error) => (None, Some(format!("Could not save the session: {error}"))),
        };
        let mut session = session.unwrap_or(final_view);
        if status == "done"
            && let Some(save_error) = &save_error
        {
            session.items.push(json!({
                "kind": "notice",
                "tone": Tone::Error,
                "text": format!(
                    "The reply finished but could not be saved: {save_error}"
                ),
            }));
        }

        {
            let mut live = self.state.chat.live();
            if save_error.is_some() {
                live.unsaved
                    .insert(self.run_id.clone(), (self.window_label.clone(), end));
            }
            live.runs.remove(&self.run_id);
        }

        if let Some(save_error) = &save_error {
            crate::log::key(format_args!("[oaklet] {save_error}"));
        }

        match status {
            "done" => {
                if let Some(save_error) = save_error {
                    self.emit(
                        "agent:error",
                        ErrorEvent {
                            session_id: self.session_id.clone(),
                            run_id: self.run_id.clone(),
                            message: format!(
                                "The reply finished but could not be saved: {save_error}"
                            ),
                            session: Some(session),
                        },
                    );
                } else {
                    self.emit(
                        "agent:done",
                        DoneEvent {
                            session_id: self.session_id.clone(),
                            run_id: self.run_id.clone(),
                            session: Some(session),
                        },
                    );
                }
            }
            "error" => self.emit(
                "agent:error",
                ErrorEvent {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    message: error.unwrap_or_else(|| "The run failed.".into()),
                    session: Some(session),
                },
            ),
            _ => self.emit(
                "agent:stopped",
                StoppedEvent {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    session: Some(session),
                },
            ),
        }

        // Anything that settled after this run's last step still reaches the
        // session, in one turn instead of one per job.
        if status != "stopped" {
            resume_notices(self);
        }
    }

    /// Persist a pending plan when this turn ended in planning rather than edits.
    fn record_plan(&self) {
        if !self.records_plan {
            return;
        }
        let Some(assessment) = self.access().assessment() else {
            return;
        };
        if !assessment.needs_plan() {
            return;
        }
        let steps = self.last_assistant_text();
        let validation = self
            .plan_validation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .map(Box::new);
        let details = self
            .plan_details
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .map(Box::new);
        self.record(EventPayload::Plan {
            version: self.next_plan_version(),
            status: store::PlanStatus::Pending,
            steps,
            details,
            validation,
        });
    }

    /// The final assistant reply becomes the card's implementation steps.
    fn last_assistant_text(&self) -> String {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending
            .iter()
            .rev()
            .find_map(|payload| match payload {
                EventPayload::Message { message, display }
                    if message.get("role").and_then(Value::as_str) == Some("assistant")
                        && !display
                            .as_ref()
                            .and_then(|display| display.get("hidden"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false) =>
                {
                    message_text(message.get("content"))
                }
                _ => None,
            })
            .unwrap_or_default()
    }
}

fn message_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
