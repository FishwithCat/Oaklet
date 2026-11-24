//! Sessions and runs: the backend owns the conversation, the UI renders it.
//!
//! Commands in this module are the only way the frontend reads or changes chat
//! state. A live run keeps its authoritative [`SessionView`] in memory, so a
//! second client can hydrate through `session_get` while tokens are streaming;
//! the user turn is appended when the run starts and the rest when it ends.

pub(crate) mod commands;
mod input;
mod manual_compaction;
mod mentions;
mod notices;
mod run;
mod transcript;

pub use input::IncomingAttachment;
pub use notices::JobNotice;
pub(crate) use notices::job_settled;
pub use run::RunContext;
#[cfg(test)]
pub(crate) use run::settle_on_panic;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::store::{self, ContextUsage, RunEnd, StepUsage, Store};
use crate::tools::CancelHandle;
use transcript::{user_item, view_from_window};

const INTERRUPTED_NOTICE: &str = "The app closed while this run was working. \
    Send another message to continue.";
/// Messages one running turn may hold before it starts rejecting new ones.
const MAX_PENDING: usize = 16;

/// Where a message sent while a run is active belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    /// Join the current turn at its next step boundary.
    Steer,
    /// Start a later turn once the current one ends.
    Queue,
}

/// How the composer asks `session_send` to deliver a message.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    Normal,
    Steer,
    Queue,
}

impl SendMode {
    fn input_mode(self) -> Option<InputMode> {
        match self {
            SendMode::Normal => None,
            SendMode::Steer => Some(InputMode::Steer),
            SendMode::Queue => Some(InputMode::Queue),
        }
    }
}

/// A user message waiting for the running turn to reach a step boundary.
pub(crate) struct PendingInput {
    pub id: String,
    pub mode: InputMode,
    pub message: Value,
    pub display: Value,
    /// Creation time, retained for the durable queue planned for P2.
    #[allow(dead_code)]
    pub created_at: i64,
}

/// The pending row a client renders; model-facing content stays in Rust.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputView {
    pub id: String,
    pub mode: InputMode,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Value>,
}

/// One window of what the UI needs to render a conversation. Model-facing
/// `messages` stay in Rust so long base64 image history never crosses IPC on
/// every turn.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub updated_at: i64,
    pub turns: usize,
    pub items: Vec<Value>,
    /// Messages sent while this session's run was busy, in arrival order.
    pub pending: Vec<PendingInputView>,
    /// Seq of the oldest event in this window, or `None` for an empty session.
    pub first_seq: Option<i64>,
    /// Whether older events exist before `first_seq`.
    pub has_more: bool,
    pub context: Option<ContextUsage>,
    /// Newest model request; the composer shows its cache share on reopen.
    pub last_usage: Option<StepUsage>,
    pub running: bool,
    pub blocked: bool,
    pub compacting: bool,
    pub active_run_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub turns: usize,
    pub running: bool,
    pub blocked: bool,
    pub compacting: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub run_id: String,
    pub session: SessionView,
    /// True when the turn was queued on an existing run instead of starting one.
    pub queued: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactResult {
    pub session: SessionView,
    pub before: usize,
    pub after: usize,
    pub compacted: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Tone {
    Error,
    Muted,
}

/// Live sessions and runs. One lock keeps the "is this session busy?" checks
/// and the run registration atomic across clients.
#[derive(Default)]
struct ChatLive {
    runs: HashMap<String, RunHandle>,
    compacting: HashMap<String, SessionOperation>,
    confirming: HashMap<String, SessionOperation>,
    closing: HashSet<String>,
    unsaved: HashMap<String, (String, Arc<RunEnd>)>,
    /// Job notices waiting for a busy session's next step.
    notices: HashMap<String, Vec<JobNotice>>,
    /// User messages waiting for a busy session's next step or next turn.
    pending: HashMap<String, Vec<PendingInput>>,
    /// Wakes opened by job notices since the session's last user message.
    wakes: HashMap<String, u32>,
}

impl ChatLive {
    fn check_plan_confirmation(
        &self,
        session_id: &str,
        window: &str,
        cancel: &crate::tools::Cancel,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("Stopped.".into());
        }
        if self
            .confirming
            .get(session_id)
            .is_none_or(|handle| handle.window != window)
        {
            return Err("This plan confirmation is no longer active.".into());
        }
        Ok(())
    }

    fn stop_session_operations(&self, session_id: &str, window: &str) {
        for handle in [
            self.compacting.get(session_id),
            self.confirming.get(session_id),
        ]
        .into_iter()
        .flatten()
        .filter(|handle| handle.window == window)
        {
            handle.cancel();
        }
    }

    fn operations(&self) -> impl Iterator<Item = &SessionOperation> {
        self.compacting.values().chain(self.confirming.values())
    }
}

struct PlanConfirmation<'a> {
    chat: &'a ChatState,
    session_id: &'a str,
}

impl Drop for PlanConfirmation<'_> {
    fn drop(&mut self) {
        self.chat.live().confirming.remove(self.session_id);
    }
}

struct RunHandle {
    session_id: String,
    window: String,
    cancel: CancelHandle,
    view: Arc<Mutex<SessionView>>,
}

struct SessionOperation {
    window: String,
    cancel: CancelHandle,
}

impl SessionOperation {
    fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl RunHandle {
    fn view(&self) -> MutexGuard<'_, SessionView> {
        self.view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct ChatState {
    store: Store,
    live: Mutex<ChatLive>,
}

impl ChatState {
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            store: Store::in_memory().expect("in-memory session store"),
            live: Mutex::new(ChatLive::default()),
        }
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        Self::from_store(Store::open(path)?)
    }

    pub(crate) fn from_store(store: Store) -> Result<Self, String> {
        let recovered = store.interrupt_stale_runs(
            store::now_ms(),
            &json!({ "role": "notice", "tone": Tone::Error, "text": INTERRUPTED_NOTICE }),
        )?;
        if recovered > 0 {
            crate::log::key(format_args!(
                "[oaklet] marked {recovered} interrupted run(s)"
            ));
        }
        Ok(Self {
            store,
            live: Mutex::new(ChatLive::default()),
        })
    }

    fn live(&self) -> MutexGuard<'_, ChatLive> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub fn any_busy(&self) -> bool {
        let live = self.live();
        !live.runs.is_empty() || live.operations().next().is_some()
    }

    fn reserve_plan_confirmation<'a>(
        &'a self,
        session_id: &'a str,
        window: &str,
        cancel: CancelHandle,
    ) -> Result<PlanConfirmation<'a>, String> {
        let mut live = self.live();
        if live.closing.contains(window)
            || live.confirming.contains_key(session_id)
            || live.compacting.contains_key(session_id)
            || live.runs.values().any(|run| run.session_id == session_id)
        {
            return Err("This chat is already working or its window is closing.".into());
        }
        live.confirming.insert(
            session_id.to_string(),
            SessionOperation {
                window: window.to_string(),
                cancel,
            },
        );
        Ok(PlanConfirmation {
            chat: self,
            session_id,
        })
    }

    /// Take the job notices queued while a run of this session was busy.
    pub fn take_notices(&self, session_id: &str) -> Vec<JobNotice> {
        self.live().notices.remove(session_id).unwrap_or_default()
    }

    /// Queue a message for a busy session. Returns the turn unchanged when the
    /// session went idle, so the caller can start a normal run instead.
    pub(crate) fn enqueue_input<F>(
        &self,
        session_id: &str,
        window: &str,
        mode: SendMode,
        id: String,
        turn: store::NewUserTurn,
        emit: F,
    ) -> Result<Result<(String, SessionView), store::NewUserTurn>, String>
    where
        F: FnOnce(&SessionView),
    {
        let Some(mode) = mode.input_mode() else {
            return Ok(Err(turn));
        };
        let mut live = self.live();
        let Some((run_id, run_window, view)) = live.runs.iter().find_map(|(run_id, handle)| {
            (handle.session_id == session_id)
                .then(|| (run_id.clone(), handle.window.clone(), handle.view.clone()))
        }) else {
            return Ok(Err(turn));
        };
        if run_window != window {
            return Err("This chat is running in another window.".into());
        }
        if live.closing.contains(window) {
            return Err("This window is closing or has an unsaved reply. \
                 Retry closing it to save the chat."
                .into());
        }
        let pending = live.pending.entry(session_id.to_string()).or_default();
        if pending.len() >= MAX_PENDING {
            return Err(format!(
                "At most {MAX_PENDING} messages can wait for this turn. Let the agent catch up."
            ));
        }
        let display = turn.display.unwrap_or_else(|| json!({ "text": "" }));
        pending.push(PendingInput {
            id,
            mode,
            message: turn.message,
            display,
            created_at: store::now_ms(),
        });
        let views = pending_views(pending);
        let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        view.pending = views;
        // Emit under the live lock so a claim cannot land between the queue
        // update and the event that tells the client about it.
        emit(&view);
        Ok(Ok((run_id, view.clone())))
    }

    /// Take every steer message queued for this session's next step.
    pub(crate) fn take_steer_inputs<F>(
        &self,
        session_id: &str,
        emit: F,
    ) -> (Vec<PendingInput>, Vec<PendingInputView>)
    where
        F: FnOnce(&[Value], &[PendingInputView]),
    {
        let mut live = self.live();
        let view = live
            .runs
            .values()
            .find(|handle| handle.session_id == session_id)
            .map(|handle| handle.view.clone());
        let Some(queue) = live.pending.get_mut(session_id) else {
            return (Vec::new(), Vec::new());
        };
        let mut taken = Vec::new();
        let mut remaining = Vec::new();
        for input in queue.drain(..) {
            if input.mode == InputMode::Steer {
                taken.push(input);
            } else {
                remaining.push(input);
            }
        }
        *queue = remaining;
        let views = pending_views(queue);
        let mut items = Vec::new();
        if let Some(view) = view {
            let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for input in &taken {
                let text = input
                    .display
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let item = user_item(text, Some(&input.display));
                view.items.push(item.clone());
                items.push(item);
            }
            view.pending = views.clone();
        }
        if !taken.is_empty() {
            // Same lock that moved the rows out of the queue publishes them,
            // so a concurrent enqueue cannot make this event look stale.
            emit(&items, &views);
        }
        (taken, views)
    }

    pub(crate) fn has_pending_steer(&self, session_id: &str) -> bool {
        self.live()
            .pending
            .get(session_id)
            .is_some_and(|pending| pending.iter().any(|input| input.mode == InputMode::Steer))
    }

    pub(crate) fn pending_views(&self, session_id: &str) -> Vec<PendingInputView> {
        let live = self.live();
        live.pending
            .get(session_id)
            .map(|pending| pending_views(pending))
            .unwrap_or_default()
    }

    /// True while any chat in this window is still producing a turn.
    pub fn window_busy(&self, window: &str) -> bool {
        let live = self.live();
        live.runs.values().any(|run| run.window == window)
            || live.operations().any(|handle| handle.window == window)
    }

    /// Wait for cancelled work to finish saving, retrying any earlier failed writes.
    pub async fn drain(&self, window: Option<&str>) -> Result<(), String> {
        loop {
            let busy = {
                let live = self.live();
                live.runs
                    .values()
                    .any(|run| window.is_none_or(|label| run.window == label))
                    || live
                        .operations()
                        .any(|handle| window.is_none_or(|label| handle.window == label))
            };
            if !busy {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let unsaved: Vec<_> = self
            .live()
            .unsaved
            .iter()
            .filter(|(_, (label, _))| window.is_none_or(|window| label == window))
            .map(|(id, (_, end))| (id.clone(), end.clone()))
            .collect();
        for (id, end) in unsaved {
            let store = self.store.clone();
            tauri::async_runtime::spawn_blocking(move || store.finish_run(&end))
                .await
                .map_err(|error| format!("Could not save the chat: {error}"))??;
            self.live().unsaved.remove(&id);
        }
        Ok(())
    }

    fn view(&self, window: store::SessionWindow) -> SessionView {
        let (compacting, pending) = {
            let live = self.live();
            (
                live.compacting.contains_key(&window.id),
                live.pending
                    .get(&window.id)
                    .map(|queue| pending_views(queue)),
            )
        };
        let mut view = view_from_window(window, compacting);
        view.pending = pending.unwrap_or_default();
        view
    }

    fn live_view(&self, session_id: &str) -> Option<SessionView> {
        let live = self.live();
        let handle = live
            .runs
            .values()
            .find(|run| run.session_id == session_id)?;
        Some(handle.view().clone())
    }

    fn summaries(&self, rows: Vec<store::SessionSummary>) -> Vec<SessionSummary> {
        let live = self.live();
        let mut active: HashMap<&str, bool> = HashMap::new();
        for handle in live.runs.values() {
            let blocked = handle.view().blocked;
            active.insert(handle.session_id.as_str(), blocked);
        }

        rows.into_iter()
            .map(|row| {
                let blocked = active.get(row.id.as_str()).copied();
                let compacting = live.compacting.contains_key(&row.id);
                SessionSummary {
                    id: row.id,
                    title: row.title,
                    updated_at: row.updated_at,
                    turns: row.turns,
                    running: blocked.is_some(),
                    blocked: blocked.unwrap_or(false),
                    compacting,
                }
            })
            .collect()
    }
}

fn pending_views(inputs: &[PendingInput]) -> Vec<PendingInputView> {
    inputs.iter().map(pending_view).collect()
}

fn pending_view(input: &PendingInput) -> PendingInputView {
    PendingInputView {
        id: input.id.clone(),
        mode: input.mode,
        text: input
            .display
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        attachments: input
            .display
            .get("attachments")
            .filter(|value| value.as_array().is_some_and(|list| !list.is_empty()))
            .cloned(),
    }
}

/// Cancel every live run, for a workspace switch or app exit.
pub fn stop_all(state: &AppState) {
    let run_ids: Vec<String> = {
        let live = state.chat.live();
        let run_ids = live
            .runs
            .iter()
            .map(|(run_id, handle)| {
                handle.cancel.cancel();
                run_id.clone()
            })
            .collect::<Vec<_>>();
        for handle in live.operations() {
            handle.cancel();
        }
        run_ids
    };
    for run_id in run_ids {
        resolve_approvals(state, &run_id);
    }
}

/// Cancel every live run and compaction owned by one window.
pub fn stop_window(state: &AppState, label: &str) {
    let run_ids = {
        let mut live = state.chat.live();
        live.closing.insert(label.to_string());
        let run_ids: Vec<String> = live
            .runs
            .iter()
            .filter(|(_, handle)| handle.window == label)
            .map(|(run_id, handle)| {
                handle.cancel.cancel();
                run_id.clone()
            })
            .collect();
        for handle in live.operations().filter(|handle| handle.window == label) {
            handle.cancel();
        }
        run_ids
    };
    for run_id in run_ids {
        resolve_approvals(state, &run_id);
    }
}

fn resolve_approvals(state: &AppState, run_id: &str) {
    let mut approvals = match state.approvals.lock() {
        Ok(approvals) => approvals,
        Err(_) => return,
    };
    let ids: Vec<String> = approvals
        .iter()
        .filter(|(_, pending)| pending.run_id == run_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some(pending) = approvals.remove(&id) {
            let _ = pending.sender.send(false);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
