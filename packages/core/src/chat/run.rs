//! Register runs and record the model-facing messages produced by the agent.
mod finish;
mod stream;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};

use super::transcript::{settle_streaming, view_from_window};
use super::{InputMode, JobNotice, PendingInputView, RunHandle, SessionView, Tone, mentions};
use crate::agent::Usage;
use crate::store::{self, EventPayload, NewUserTurn};
use crate::tools::{Access, CancelHandle};
use crate::{SharedState, context};

/// What opened a run. Background notices never unlock execution tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunTrigger {
    User,
    /// The user typed the `@arch` mention: plan only, never edit.
    Arch,
    Worktree(bool),
    Notice,
    /// The user confirmed a plan card; the run consumes that version and may
    /// edit from its first step.
    Plan(u64),
}

enum RunInput {
    Direct(NewUserTurn),
    Queued,
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
) -> Result<Arc<Mutex<SessionView>>, String> {
    let run = prepare_run(
        state,
        window,
        session_id,
        trigger,
        config,
        RunInput::Direct(turn),
        cancel,
        cancel_handle,
    )?
    .expect("A direct run always has an input");
    let view = run.ctx.view.clone();
    run.spawn(app.clone());
    Ok(view)
}

pub(super) fn resume_queued(
    app: &AppHandle,
    state: &SharedState,
    window: &str,
    session_id: &str,
) -> Result<Option<Arc<Mutex<SessionView>>>, String> {
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    let store = state.chat.store.clone();
    let meta = store
        .session_meta(session_id)?
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    config.workspace = store.worktree_directory(session_id, &meta.workspace)?;
    let (cancel_handle, cancel) = CancelHandle::new();
    let Some(run) = prepare_run(
        state,
        window,
        session_id,
        RunTrigger::User,
        config,
        RunInput::Queued,
        cancel,
        cancel_handle,
    )?
    else {
        return Ok(None);
    };
    let view = run.ctx.view.clone();
    run.spawn(app.clone());
    Ok(Some(view))
}

struct PreparedRun {
    ctx: RunContext,
    config: crate::AppConfig,
    messages: Vec<Value>,
    cancel: crate::tools::Cancel,
}

#[allow(clippy::too_many_arguments)]
fn prepare_run(
    state: &SharedState,
    window: &str,
    session_id: &str,
    trigger: RunTrigger,
    config: crate::AppConfig,
    input: RunInput,
    cancel: crate::tools::Cancel,
    cancel_handle: CancelHandle,
) -> Result<Option<PreparedRun>, String> {
    let store = state.chat.store.clone();
    let run_id = state.next_id();
    let selected_model = {
        let workspace = std::fs::canonicalize(&config.workspace)
            .unwrap_or_else(|_| PathBuf::from(&config.workspace));
        let workspace = workspace.to_string_lossy().into_owned();
        state
            .architecture
            .lock()
            .ok()
            .and_then(|cache| cache.selection(&workspace))
    };

    // Hold the live lock across the short SQLite write so two clients cannot
    // both start a run for the same session.
    let auto_resume = matches!(input, RunInput::Queued);
    let (view, start_seq, messages, access, plan_state, plan_turn) = {
        let mut live = state.chat.live();
        let (turn, trigger) = match input {
            RunInput::Direct(turn) => (turn, trigger),
            RunInput::Queued => {
                let Some(input) = live.pending.get(session_id).and_then(|queue| queue.first())
                else {
                    return Ok(None);
                };
                let turn = NewUserTurn {
                    message: input.message.clone(),
                    display: Some(input.display.clone()),
                    title: None,
                };
                let trigger = if input.mode == InputMode::Queue
                    && input.display["text"]
                        .as_str()
                        .is_some_and(|text| mentions::has(text, "arch"))
                {
                    RunTrigger::Arch
                } else {
                    trigger
                };
                (turn, trigger)
            }
        };
        let reserved = matches!(trigger, RunTrigger::Plan(_) | RunTrigger::Worktree(_));
        if reserved {
            live.check_plan_confirmation(session_id, window, &cancel)?;
        }
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
            || (!reserved && live.confirming.contains_key(session_id))
        {
            return Err("This chat is already working.".into());
        }
        // A notice wake must not run while a plan awaits review. The live lock
        // keeps this check and the registration atomic against a finishing run.
        let plan = store.plan_state(session_id)?;
        if trigger == RunTrigger::Notice && plan.pending {
            return Err("A plan is waiting for review.".into());
        }
        // A plan is under review, or the user typed `@arch`. Confirming a plan
        // is an implementation run and also needs the model scope.
        let plan_turn = plans_turn(trigger, plan.pending);
        let started = match trigger {
            RunTrigger::Plan(version) => {
                store.begin_plan_run(session_id, &run_id, &turn, version, store::now_ms())?
            }
            RunTrigger::User | RunTrigger::Arch | RunTrigger::Worktree(_) | RunTrigger::Notice => {
                store.begin_run(session_id, &run_id, &turn, store::now_ms())?
            }
        };
        if auto_resume {
            let queue = live
                .pending
                .get_mut(session_id)
                .expect("Queued input under the live lock");
            queue.remove(0);
            if queue.is_empty() {
                live.pending.remove(session_id);
            }
        }
        // `prior_messages` is the surface before this user turn, read in the
        // same transaction that appended it. Trim an incomplete tool block
        // before adding the new turn so the request stays valid.
        let mut messages = trim_incomplete_tool_block(started.prior_messages);
        messages.push(context::identify(
            turn.message.clone(),
            context::MessageId::Stored(started.start_seq),
        ));
        let mut view = view_from_window(started.window, false);
        view.pending = super::pending_views(
            live.pending
                .get(session_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
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
        (
            view,
            started.start_seq,
            messages,
            access,
            started.plan,
            plan_turn,
        )
    };
    let planning = if plan_turn { selected_model } else { None };

    let ctx = RunContext {
        app: None,
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
        planning,
        confirmed_plan: match trigger {
            RunTrigger::Plan(version) => Some(version),
            RunTrigger::User | RunTrigger::Arch | RunTrigger::Worktree(_) | RunTrigger::Notice => {
                None
            }
        },
        plan_validation: Arc::new(Mutex::new(None)),
        plan_details: Arc::new(Mutex::new(None)),
        next_plan_version: plan_state.latest.unwrap_or(0) + 1,
        plan_requires_validation: plan_state.requires_validation,
        records_plan: plan_turn,
        auto_resume,
    };
    Ok(Some(PreparedRun {
        ctx,
        config,
        messages,
        cancel,
    }))
}

impl PreparedRun {
    fn spawn(self, app: AppHandle) {
        let Self {
            mut ctx,
            config,
            messages,
            cancel,
        } = self;
        ctx.app = Some(app);
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
    }
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
    /// Architecture model selected when the run started.
    planning: Option<crate::architecture::PlanningScope>,
    /// Plan version the user approved for this run, if any.
    confirmed_plan: Option<u64>,
    /// Latest draft validation, attached to the plan when the turn finishes.
    plan_validation: Arc<Mutex<Option<store::PlanValidation>>>,
    /// Structured review content submitted with `present_plan`.
    plan_details: Arc<Mutex<Option<store::PlanDetails>>>,
    /// Version the next plan recorded by this run will carry.
    next_plan_version: u64,
    plan_requires_validation: bool,
    /// Background-notice turns never create a plan awaiting review.
    records_plan: bool,
    /// This turn was opened to deliver a message left after an earlier run, so
    /// an error must not start another one.
    auto_resume: bool,
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

    /// User messages that joined the turn at this step boundary.
    pub fn take_steer_inputs(&self) -> Vec<super::PendingInput> {
        let (taken, _) = self
            .state
            .chat
            .take_steer_inputs(&self.session_id, |items, pending| {
                self.emit(
                    "agent:steered",
                    SteeredEvent {
                        session_id: self.session_id.clone(),
                        run_id: self.run_id.clone(),
                        items: items.to_vec(),
                        pending: pending.to_vec(),
                    },
                );
            });
        taken
    }

    /// True when a steer arrived after this step's request was sent.
    pub fn has_pending_steer(&self) -> bool {
        self.state.chat.has_pending_steer(&self.session_id)
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

    pub fn planning_scope(&self) -> Option<&crate::architecture::PlanningScope> {
        self.planning.as_ref()
    }

    pub fn confirmed_plan(&self) -> Option<u64> {
        self.confirmed_plan
    }

    pub fn plan_validation_slot(&self) -> Arc<Mutex<Option<store::PlanValidation>>> {
        self.plan_validation.clone()
    }

    pub fn plan_details_slot(&self) -> Arc<Mutex<Option<store::PlanDetails>>> {
        self.plan_details.clone()
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
            planning: None,
            confirmed_plan: None,
            plan_validation: Arc::new(Mutex::new(None)),
            plan_details: Arc::new(Mutex::new(None)),
            next_plan_version: started.plan.latest.unwrap_or(0) + 1,
            plan_requires_validation: started.plan.requires_validation,
            records_plan: true,
            auto_resume: false,
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SteeredEvent {
    session_id: String,
    run_id: String,
    items: Vec<Value>,
    pending: Vec<PendingInputView>,
}

/// A plain user turn executes normally. `@arch` and plan revisions are
/// planning-only; a reviewed plan starts writable and can return to planning if
/// the work grows beyond the approved scope.
fn plans_turn(trigger: RunTrigger, pending_plan: bool) -> bool {
    matches!(
        trigger,
        RunTrigger::Arch | RunTrigger::Plan(_) | RunTrigger::Worktree(true)
    ) || (matches!(trigger, RunTrigger::User | RunTrigger::Worktree(false)) && pending_plan)
}

fn access_for(trigger: RunTrigger, pending_plan: bool) -> Arc<Access> {
    match trigger {
        RunTrigger::Plan(_) => Access::approved(),
        RunTrigger::Notice => Access::locked_read_only(),
        RunTrigger::Arch | RunTrigger::Worktree(true) => Access::planning(),
        // A follow-up on a plan under review keeps the turn read-only even
        // without the mention, so revisions cannot edit before confirmation.
        RunTrigger::User | RunTrigger::Worktree(false) if pending_plan => Access::planning(),
        // Every other user turn behaves like the normal execution flow.
        RunTrigger::User | RunTrigger::Worktree(false) => Access::execute(),
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
    fn plain_turns_execute_and_arch_turns_stay_read_only() {
        assert!(!plans_turn(RunTrigger::User, false));
        assert!(plans_turn(RunTrigger::Arch, false));
        assert!(plans_turn(RunTrigger::User, true));
        assert!(plans_turn(RunTrigger::Plan(1), false));
        assert!(!plans_turn(RunTrigger::Notice, true));

        let access = access_for(RunTrigger::User, false);
        assert!(!access.is_locked());
        assert_eq!(access.mode(), ToolAccess::Execute);
        assert_eq!(
            access.assess(Assessment {
                intent: Intent::Implement,
                impact: ArchitectureImpact::None,
            }),
            ToolAccess::Execute
        );

        for (trigger, pending) in [(RunTrigger::Arch, false), (RunTrigger::User, true)] {
            let access = access_for(trigger, pending);
            assert!(!access.is_locked());
            assert!(access.planning_only());
            assert_eq!(access.mode(), ToolAccess::ReadOnly);
            assert_eq!(
                access.assess(Assessment {
                    intent: Intent::Implement,
                    impact: ArchitectureImpact::None,
                }),
                ToolAccess::ReadOnly
            );
            assert!(access.assessment().is_some_and(Assessment::needs_plan));
        }

        let notice = access_for(RunTrigger::Notice, false);
        assert!(notice.is_locked());
        assert_eq!(
            notice.assess(Assessment {
                intent: Intent::Implement,
                impact: ArchitectureImpact::None,
            }),
            ToolAccess::ReadOnly
        );

        let approved = access_for(RunTrigger::Plan(1), true);
        assert!(approved.is_locked());
        assert_eq!(approved.mode(), ToolAccess::Execute);
        assert_eq!(
            approved.assess(Assessment {
                intent: Intent::Implement,
                impact: ArchitectureImpact::Confirmed,
            }),
            ToolAccess::ReadOnly
        );
        assert_eq!(
            approved.assess(Assessment {
                intent: Intent::Implement,
                impact: ArchitectureImpact::None
            }),
            ToolAccess::ReadOnly
        );
        assert!(approved.assessment().is_some_and(Assessment::needs_plan));
    }
}
