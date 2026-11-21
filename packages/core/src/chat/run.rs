//! Register runs and record the model-facing messages produced by the agent.
mod finish;
mod stream;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};

use super::transcript::{settle_streaming, view_from_window};
use super::{JobNotice, RunHandle, SessionView, Tone};
use crate::agent::Usage;
use crate::store::{self, EventPayload, NewUserTurn};
use crate::tools::{Access, CancelHandle};
use crate::{SharedState, context};

/// What opened a run. Background notices never unlock execution tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunTrigger {
    User,
    Notice,
}

/// Register a run for an already-built user turn and spawn the agent loop. The
/// caller decides what the turn means: a typed message or a job notice.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_run(
    app: &AppHandle,
    state: &SharedState,
    window: &str,
    session_id: &str,
    trigger: RunTrigger,
    config: crate::AppConfig,
    turn: NewUserTurn,
    cancel: crate::tools::Cancel,
    cancel_handle: CancelHandle,
) -> Result<(String, Arc<Mutex<SessionView>>), String> {
    let store = state.chat.store.clone();
    let run_id = state.next_id();

    // Hold the live lock across the short SQLite write so two clients cannot
    // both start a run for the same session.
    let (view, start_seq, messages, access, next_plan_version) = {
        let mut live = state.chat.live();
        if live.closing.contains(window)
            || live
                .unsaved
                .values()
                .any(|(_, end)| end.session_id == session_id)
        {
            return Err("This window is closing or has an unsaved reply. Retry closing it to save the chat.".into());
        }
        if live.compacting.contains_key(session_id)
            || live.runs.values().any(|run| run.session_id == session_id)
        {
            return Err("This chat is already working.".into());
        }
        // A notice wake must not run while a plan awaits review. The live lock
        // keeps this check and the registration atomic against a finishing run.
        let plan = store.plan_state(session_id)?;
        if trigger == RunTrigger::Notice && plan.pending {
            return Err("A plan is waiting for review.".into());
        }
        let started = store.begin_run(session_id, &run_id, &turn, store::now_ms())?;
        // `prior_messages` is the surface before this user turn, read in the
        // same transaction that appended it. Trim an incomplete tool block
        // before adding the new turn so the request stays valid.
        let mut messages = trim_incomplete_tool_block(started.prior_messages);
        messages.push(context::identify(
            turn.message.clone(),
            context::MessageId::Stored(started.start_seq),
        ));
        let mut view = view_from_window(started.window, false);
        view.running = true;
        view.active_run_id = Some(run_id.clone());
        let view = Arc::new(Mutex::new(view));
        live.runs.insert(
            run_id.clone(),
            RunHandle {
                session_id: session_id.to_string(),
                window: window.to_string(),
                cancel: cancel_handle,
                view: view.clone(),
            },
        );
        let access = access_for(trigger, started.plan.pending);
        let next_plan_version = started.plan.latest.unwrap_or(0) + 1;
        (view, started.start_seq, messages, access, next_plan_version)
    };

    let ctx = RunContext {
        app: Some(app.clone()),
        state: state.clone(),
        session_id: session_id.to_string(),
        run_id: run_id.clone(),
        window_label: window.to_string(),
        start_seq,
        view: view.clone(),
        finished: Arc::new(AtomicBool::new(false)),
        archived: Arc::new(Mutex::new(Vec::new())),
        pending: Arc::new(Mutex::new(Vec::new())),
        partial: Arc::new(Mutex::new(PartialReply::default())),
        compacted: Arc::new(AtomicBool::new(false)),
        compaction_sizes: Arc::new(Mutex::new(None)),
        usage: Arc::new(Mutex::new(store::RunUsage::default())),
        access,
        next_plan_version,
        records_plan: trigger == RunTrigger::User,
    };
    let recovery_ctx = ctx.clone();
    let recovery_messages = messages.clone();
    tauri::async_runtime::spawn(async move {
        settle_on_panic(
            crate::agent::run(config, messages, cancel, ctx),
            recovery_ctx,
            recovery_messages,
        )
        .await;
    });
    Ok((run_id, view))
}

/// Keep a panicking agent task from leaving its run stuck as `running`.
///
/// The task has no error channel of its own, so a panic would otherwise leave
/// the transcript open and the UI waiting forever. Turn it into the same
/// terminal error a failed request produces.
///
/// This requires unwinding; the release profile's `panic = "abort"` still
/// aborts the process instead.
pub(crate) async fn settle_on_panic<F>(run: F, ctx: RunContext, messages: Vec<Value>)
where
    F: std::future::Future<Output = ()>,
{
    if let Err(payload) = std::panic::AssertUnwindSafe(run).catch_unwind().await {
        let detail = panic_detail(payload.as_ref());
        crate::log::key(format_args!("[oaklet] agent task panicked: {detail}"));
        ctx.finish_error(
            messages,
            format!("The agent stopped unexpectedly: {detail}"),
        )
        .await;
    }
}

fn panic_detail(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Close an incomplete tool-call block in model history without truncating
/// later user turns. The database is repaired separately at run finish; this
/// keeps replayed/legacy histories valid before a request.
pub(super) fn trim_incomplete_tool_block(mut messages: Vec<Value>) -> Vec<Value> {
    let Some(index) = messages.iter().rposition(|message| {
        message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
    }) else {
        return messages;
    };
    let expected = messages[index]
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if expected == 0 {
        return messages;
    }

    let calls: Vec<(String, String)> = messages[index]
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            Some((
                call.get("id").and_then(Value::as_str)?.to_string(),
                call.pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string(),
            ))
        })
        .collect();
    let answered: HashSet<&str> = messages[index + 1..]
        .iter()
        .filter_map(|message| {
            (message.get("role").and_then(Value::as_str) == Some("tool"))
                .then(|| message.get("tool_call_id").and_then(Value::as_str))
                .flatten()
        })
        .collect();
    let missing: Vec<&(String, String)> = calls
        .iter()
        .filter(|(id, _)| !answered.contains(id.as_str()))
        .collect();
    if missing.is_empty() {
        return messages;
    }

    // Keep the synthetic results next to the tool block they answer. Later
    // user/assistant turns stay untouched and the request remains valid.
    let block_ids: HashSet<&str> = calls.iter().map(|(id, _)| id.as_str()).collect();
    let mut insert_at = index + 1;
    for (offset, message) in messages[index + 1..].iter().enumerate() {
        let Some(id) = message.get("tool_call_id").and_then(Value::as_str) else {
            break;
        };
        if message.get("role").and_then(Value::as_str) != Some("tool") || !block_ids.contains(id) {
            break;
        }
        insert_at = index + 2 + offset;
    }

    let synthetic: Vec<Value> = missing
        .into_iter()
        .map(|(id, _)| {
            let started = messages[index + 1..].iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message.get("tool_call_id").and_then(Value::as_str) == Some(id)
            });
            let text = if started {
                "The tool call was interrupted after it started, but no result was recorded. \
                 Its outcome is unknown; verify external state before retrying."
            } else {
                "The tool call was interrupted before it started. Retry it if it is still needed."
            };
            json!({
                "role": "tool",
                "tool_call_id": id,
                "content": text,
            })
        })
        .collect();
    let identified = context::message_id(&messages[index]).is_ok();
    let synthetic = synthetic.into_iter().map(|message| {
        if identified {
            let id = message["tool_call_id"].as_str().unwrap_or("").to_string();
            context::identify(
                context::source(message, "repair"),
                context::MessageId::Repair(id),
            )
        } else {
            message
        }
    });
    messages.splice(insert_at..insert_at, synthetic);
    messages
}

/// Everything the agent loop needs to record and emit one run.
#[derive(Clone)]
pub struct RunContext {
    app: Option<AppHandle>,
    state: SharedState,
    session_id: String,
    run_id: String,
    window_label: String,
    start_seq: i64,
    view: Arc<Mutex<SessionView>>,
    finished: Arc<AtomicBool>,
    /// Originals replaced by automatic compaction during this run, exposed to
    /// `history_read` until the compaction record is durable.
    archived: Arc<Mutex<Vec<(i64, Value)>>>,
    /// Append-only events this run produced, in order.
    pending: Arc<Mutex<Vec<EventPayload>>>,
    /// Text from the current model stream, kept until the stream settles.
    partial: Arc<Mutex<PartialReply>>,
    /// Set once compaction replaced part of the model history.
    compacted: Arc<AtomicBool>,
    /// Local request sizes at the first and last compaction in this run.
    compaction_sizes: Arc<Mutex<Option<(usize, usize)>>>,
    /// Provider-reported tokens accumulated across this run's model steps.
    usage: Arc<Mutex<store::RunUsage>>,
    /// Capability gate shared with the tool registry.
    access: Arc<Access>,
    /// Version the next plan recorded by this run will carry.
    next_plan_version: u64,
    /// Background-notice turns never create a plan awaiting review.
    records_plan: bool,
}

#[derive(Default)]
struct PartialReply {
    content: String,
    reasoning: String,
    reasoning_started_at: Option<u64>,
    reasoning_duration_ms: Option<u64>,
}

impl PartialReply {
    fn settle_reasoning(&mut self, now: u64) {
        if let Some(started_at) = self.reasoning_started_at.take() {
            self.reasoning_duration_ms =
                Some(self.reasoning_duration_ms.unwrap_or(0) + now.saturating_sub(started_at));
        }
    }
}

impl RunContext {
    pub fn app(&self) -> &AppHandle {
        self.app.as_ref().expect("Desktop runs have an app handle")
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_label(&self) -> &str {
        &self.window_label
    }

    /// Job notices that settled while this run was busy; they join the next
    /// model step.
    pub fn take_notices(&self) -> Vec<JobNotice> {
        self.state.chat.take_notices(&self.session_id)
    }

    pub fn store(&self) -> crate::store::Store {
        self.state.chat.store.clone()
    }

    pub fn jobs(&self) -> std::sync::Arc<crate::tools::jobs::JobRegistry> {
        self.state.jobs.clone()
    }

    pub fn access(&self) -> &Arc<Access> {
        &self.access
    }

    pub fn next_plan_version(&self) -> u64 {
        self.next_plan_version
    }

    pub fn archived(&self) -> Arc<Mutex<Vec<(i64, Value)>>> {
        self.archived.clone()
    }

    pub fn mark_compacted(&self, before: usize, after: usize) {
        self.compacted.store(true, Ordering::SeqCst);
        let mut recorded = self
            .compaction_sizes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let first = recorded.map(|(first, _)| first).unwrap_or(before);
        *recorded = Some((first, after));
    }

    /// Accumulate one successful model step and publish the new total.
    pub fn record_usage(&self, usage: Option<Usage>) {
        let mut total = self
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        total.add(usage);
        let snapshot = *total;
        drop(total);
        self.emit_usage(snapshot);
    }

    pub(crate) fn usage(&self) -> store::RunUsage {
        *self
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record(&self, payload: EventPayload) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.push(payload);
    }

    /// Record a model-visible message plus its optional UI metadata.
    pub fn record_message(&self, message: Value, mut display: Option<Value>) -> Value {
        if message.get("role").and_then(Value::as_str) == Some("assistant") {
            let mut partial =
                std::mem::take(&mut *self.partial.lock().unwrap_or_else(|p| p.into_inner()));
            partial.settle_reasoning(store::now_ms() as u64);
            if let Some(duration) = partial.reasoning_duration_ms {
                display.get_or_insert_with(|| json!({}))["reasoningDurationMs"] = json!(duration);
            }
            self.update_view(|view| settle_streaming(&mut view.items));
        }
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let message = context::identify(message, context::MessageId::Pending(pending.len()));
        pending.push(EventPayload::Message {
            message: message.clone(),
            display,
        });
        message
    }

    /// Record a model-only turn (tool images, reminders) that the UI hides.
    pub fn record_hidden(&self, message: Value) -> Value {
        self.record_message(message, Some(json!({ "hidden": true })))
    }

    fn record_notice(&self, tone: Tone, text: impl Into<String>) {
        self.record(EventPayload::Notice {
            tone: match tone {
                Tone::Error => "error",
                Tone::Muted => "muted",
            }
            .into(),
            text: text.into(),
        });
    }

    /// Project live originals with the same event identities used at finish.
    pub async fn record_archived(&self, surface: &[Value]) -> Result<(), String> {
        let appended = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let surface = surface
            .iter()
            .filter(|message| message["role"] != "system")
            .cloned()
            .collect::<Vec<_>>();
        let store = self.store();
        let id = self.session_id.clone();
        let rows = tauri::async_runtime::spawn_blocking(move || {
            store.pending_archive(&id, &appended, &surface)
        })
        .await
        .map_err(|error| format!("Could not read archived history: {error}"))??;
        *self.archived.lock().unwrap_or_else(|p| p.into_inner()) = rows;
        Ok(())
    }

    fn emit<T: Serialize + Clone>(&self, event: &str, payload: T) {
        if let Some(app) = &self.app {
            let _ = app.emit_to(&self.window_label, event, payload);
        }
    }

    #[cfg(test)]
    pub(crate) fn without_window(
        state: SharedState,
        session_id: &str,
        run_id: &str,
        started: store::RunStart,
    ) -> Self {
        // Exercise real requests and persistence without starting a native window.
        let mut view = view_from_window(started.window, false);
        view.running = true;
        view.active_run_id = Some(run_id.into());
        Self {
            app: None,
            state,
            session_id: session_id.into(),
            run_id: run_id.into(),
            window_label: "test".into(),
            start_seq: started.start_seq,
            view: Arc::new(Mutex::new(view)),
            finished: Arc::new(AtomicBool::new(false)),
            archived: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(Vec::new())),
            partial: Arc::new(Mutex::new(PartialReply::default())),
            compacted: Arc::new(AtomicBool::new(false)),
            compaction_sizes: Arc::new(Mutex::new(None)),
            usage: Arc::new(Mutex::new(store::RunUsage::default())),
            access: access_for(RunTrigger::User, started.plan.pending),
            next_plan_version: started.plan.latest.unwrap_or(0) + 1,
            records_plan: true,
        }
    }

    fn update_view(&self, update: impl FnOnce(&mut SessionView)) {
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut view);
    }
}

/// Background notices and reviewed plans lock the gate; a plain user turn only
/// starts read-only so `assess_change` can classify it.
fn access_for(trigger: RunTrigger, pending_plan: bool) -> Arc<Access> {
    if trigger == RunTrigger::Notice || pending_plan {
        Access::locked_read_only()
    } else {
        Access::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ArchitectureImpact, Assessment, Intent, ToolAccess};

    #[test]
    fn closes_a_tool_block_that_stopped_before_its_results() {
        let assistant_with_two_calls = json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                { "id": "a", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
                { "id": "b", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
            ],
        });
        let partial = vec![
            json!({ "role": "user", "content": "go" }),
            assistant_with_two_calls.clone(),
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
        ];
        let repaired = trim_incomplete_tool_block(partial);
        assert_eq!(repaired.len(), 4);
        assert_eq!(repaired[0]["role"], "user");
        assert_eq!(repaired[1]["role"], "assistant");
        assert_eq!(repaired[2]["tool_call_id"], "a");
        assert_eq!(repaired[3]["tool_call_id"], "b");
        assert!(
            repaired[3]["content"]
                .as_str()
                .is_some_and(|text| text.contains("interrupted"))
        );

        // A later turn must survive so the model does not forget it.
        let with_later_turn = vec![
            json!({ "role": "user", "content": "go" }),
            assistant_with_two_calls.clone(),
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
            json!({ "role": "user", "content": "next" }),
            json!({ "role": "assistant", "content": "answer" }),
        ];
        let repaired = trim_incomplete_tool_block(with_later_turn);
        assert_eq!(repaired.len(), 6);
        assert_eq!(repaired[3]["tool_call_id"], "b");
        assert_eq!(repaired[4]["content"], "next");
        assert_eq!(repaired[5]["content"], "answer");

        let complete = vec![
            json!({ "role": "user", "content": "go" }),
            assistant_with_two_calls,
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
            json!({ "role": "tool", "tool_call_id": "b", "content": "ok" }),
            json!({ "role": "assistant", "content": "done" }),
        ];
        assert_eq!(trim_incomplete_tool_block(complete).len(), 5);
    }

    #[test]
    fn reasoning_time_excludes_content_and_accumulates_across_segments() {
        let mut partial = super::PartialReply {
            reasoning_started_at: Some(1000),
            ..Default::default()
        };
        partial.settle_reasoning(2500);
        assert_eq!(partial.reasoning_duration_ms, Some(1500));
        partial.settle_reasoning(5000);
        assert_eq!(partial.reasoning_duration_ms, Some(1500));
        partial.reasoning_started_at = Some(6000);
        partial.settle_reasoning(7000);
        assert_eq!(partial.reasoning_duration_ms, Some(2500));
        partial.reasoning_started_at = Some(8000);
        partial.settle_reasoning(7999);
        assert_eq!(partial.reasoning_duration_ms, Some(2500));
    }

    #[test]
    fn only_user_turns_without_a_pending_plan_can_open_execution() {
        let access = access_for(RunTrigger::User, false);
        assert!(!access.is_locked());
        assert_eq!(access.mode(), ToolAccess::ReadOnly);
        assert_eq!(
            access.assess(Assessment {
                intent: Intent::Implement,
                impact: ArchitectureImpact::None,
            }),
            ToolAccess::Execute
        );

        for (trigger, pending) in [(RunTrigger::Notice, false), (RunTrigger::User, true)] {
            let access = access_for(trigger, pending);
            assert!(access.is_locked());
            assert_eq!(
                access.assess(Assessment {
                    intent: Intent::Implement,
                    impact: ArchitectureImpact::None,
                }),
                ToolAccess::ReadOnly
            );
        }
    }
}
