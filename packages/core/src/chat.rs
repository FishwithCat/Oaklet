//! Sessions and runs: the backend owns the conversation, the UI renders it.
//!
//! Commands in this module are the only way the frontend reads or changes chat
//! state. A live run keeps its authoritative [`SessionView`] in memory, so a
//! second client can hydrate through `session_get` while tokens are streaming;
//! the user turn is appended when the run starts and the rest when it ends.

pub(crate) mod commands;
mod input;
mod manual_compaction;
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

use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::store::{self, ContextUsage, RunEnd, StepUsage, Store};
use crate::tools::CancelHandle;
use transcript::view_from_window;

const INTERRUPTED_NOTICE: &str = "The app closed while this run was working. \
    Send another message to continue.";

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
    compacting: HashMap<String, CompactionHandle>,
    closing: HashSet<String>,
    unsaved: HashMap<String, (String, Arc<RunEnd>)>,
    /// Job notices waiting for a busy session's next step.
    notices: HashMap<String, Vec<JobNotice>>,
    /// Wakes opened by job notices since the session's last user message.
    wakes: HashMap<String, u32>,
}

struct RunHandle {
    session_id: String,
    window: String,
    cancel: CancelHandle,
    view: Arc<Mutex<SessionView>>,
}

struct CompactionHandle {
    window: String,
    cancel: CancelHandle,
}

impl CompactionHandle {
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

    /// Take the job notices queued while a run of this session was busy.
    pub fn take_notices(&self, session_id: &str) -> Vec<JobNotice> {
        self.live().notices.remove(session_id).unwrap_or_default()
    }

    /// True while any chat in this window is still producing a turn.
    pub fn window_busy(&self, window: &str) -> bool {
        let live = self.live();
        live.runs.values().any(|run| run.window == window)
            || live
                .compacting
                .values()
                .any(|handle| handle.window == window)
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
                        .compacting
                        .values()
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
        let compacting = self.live().compacting.contains_key(&window.id);
        view_from_window(window, compacting)
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
        for handle in live.compacting.values() {
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
        for handle in live
            .compacting
            .values()
            .filter(|handle| handle.window == label)
        {
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
