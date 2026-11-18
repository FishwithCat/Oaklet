//! Sessions and runs: the backend owns the conversation, the UI renders it.
//!
//! Commands in this module are the only way the frontend reads or changes chat
//! state. A live run keeps its authoritative [`SessionView`] in memory, so a
//! second client can hydrate through `session_get` while tokens are streaming;
//! the user turn is appended when the run starts and the rest when it ends.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::store::{self, ContextUsage, Event, EventPayload, NewUserTurn, RunEnd, Session, Store};
use crate::tools::{CancelHandle, FileDiff};
use crate::{AppState, SharedState, compaction, context, spill, workspace_for_window};

const MAX_ATTACHMENTS: usize = 20;
const MAX_FILE_BYTES: usize = 200_000;
const MAX_IMAGE_DATA_URL: usize = 8_000_000;
/// Attached text above this size is spilled instead of inlined whole.
const ATTACHMENT_SPILL_BYTES: usize = 16_000;
/// Lines kept at each end of a spilled attachment's inline preview.
const ATTACHMENT_HEAD_LINES: usize = 80;
const ATTACHMENT_TAIL_LINES: usize = 40;
/// Character caps so one very long line cannot fill the preview budget.
const ATTACHMENT_HEAD_CHARS: usize = 6_000;
const ATTACHMENT_TAIL_CHARS: usize = 2_000;
const INTERRUPTED_NOTICE: &str = "The app closed while this run was working. \
    Send another message to continue.";
/// Largest job output a completion notice carries inline; a bigger one leaves
/// the read to `job_output`.
const INLINE_NOTICE_CHARS: usize = 2_000;
/// Turns one session may open from job notices between two user messages, so a
/// job whose completion starts another job cannot wake it forever.
const MAX_WAKES: u32 = 8;
/// Longest command label shown in a notice summary.
const NOTICE_LABEL_CHARS: usize = 80;

/// A settled background job waiting for its session: the one-line transcript
/// notice and the model-facing message it carries.
#[derive(Clone)]
pub struct JobNotice {
    pub summary: String,
    pub message: Value,
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
    /// Seq of the oldest event in this window, or `None` for an empty session.
    pub first_seq: Option<i64>,
    /// Whether older events exist before `first_seq`.
    pub has_more: bool,
    pub context: Option<ContextUsage>,
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

/// A pasted file or image, matching the composer's prepared attachment.
#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IncomingAttachment {
    Image {
        name: String,
        bytes: u64,
        data_url: String,
        thumbnail: String,
    },
    File {
        name: String,
        bytes: u64,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum ToolStatus {
    Pending,
    Awaiting,
    Running,
    Ok,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolItem {
    run_id: String,
    id: String,
    name: String,
    status: ToolStatus,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diffs: Vec<FileDiff>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Tone {
    Error,
    Muted,
}

/// A transcript row the backend creates. Streaming assistant/reasoning rows are
/// built directly as JSON so they can be merged in place.
#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum NewItem {
    Tool(Box<ToolItem>),
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum SentAttachment {
    Image {
        name: String,
        bytes: u64,
        thumbnail: String,
    },
    File {
        name: String,
        bytes: u64,
    },
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

    #[cfg(test)]
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

fn view_from_session(session: Session, compacting: bool) -> SessionView {
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
        running: false,
        blocked: false,
        compacting,
        active_run_id: None,
    }
}

fn view_from_window(window: store::SessionWindow, compacting: bool) -> SessionView {
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
    let mut items = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for event in events {
        match &event.payload {
            EventPayload::ToolTrim { .. } => {},
            EventPayload::Notice { tone, text } => items.push(json!({
                "kind": "notice",
                "seq": event.seq,
                "tone": tone,
                "text": text,
            })),
            EventPayload::Compaction { before, after, .. } | EventPayload::Context { before, after, .. } => items.push(json!({
                "kind": "notice",
                "seq": event.seq,
                "tone": "muted",
                "text": format!(
                    "Compacted context: {} → {} tokens. Earlier messages stay readable with history_read.",
                    format_tokens(*before),
                    format_tokens(*after)
                ),
            })),
            EventPayload::Message {
                message,
                display,
            } => {
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

/// Model-facing history: the latest summary plus every message after its
/// boundary. Display-only data never crosses this line.
#[cfg(test)]
fn surface_messages(events: &[Event]) -> Vec<Value> {
    let through = events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::Compaction {
            shadowed_through, ..
        } => Some(*shadowed_through),
        _ => None,
    });
    let start = through.unwrap_or(0);
    let mut messages = Vec::new();
    if through.is_some()
        && let Some(EventPayload::Compaction { summary, .. }) = events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, EventPayload::Compaction { .. }))
            .map(|event| &event.payload)
    {
        messages.push(store::checkpoint(summary.clone()));
    }
    let trims = store::tool_trims(events);
    messages.extend(events.iter().filter_map(|event| match &event.payload {
        EventPayload::Message { message, .. } if event.seq > start => {
            Some(trims.get(&event.seq).unwrap_or(message).clone())
        }
        _ => None,
    }));
    trim_incomplete_tool_block(messages)
}

#[tauri::command]
pub async fn session_create(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<SessionView, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    let store = state.chat.store.clone();
    let id = state.next_id();
    let now = store::now_ms();
    let session = tauri::async_runtime::spawn_blocking(move || {
        store.create_session(&id, &workspace.to_string_lossy(), now)
    })
    .await
    .map_err(|error| format!("Could not create the session: {error}"))??;
    Ok(view_from_session(session, false))
}

#[tauri::command]
pub async fn session_list(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<Vec<SessionSummary>, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    let store = state.chat.store.clone();
    let rows = tauri::async_runtime::spawn_blocking(move || {
        store.list_sessions(&workspace.to_string_lossy())
    })
    .await
    .map_err(|error| format!("Could not read session history: {error}"))??;
    Ok(state.chat.summaries(rows))
}

#[tauri::command]
pub async fn session_get(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    id: String,
    before_seq: Option<i64>,
) -> Result<SessionView, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    if before_seq.is_none()
        && let Some(view) = state.chat.live_view(&id)
    {
        if Path::new(&view.workspace) != workspace {
            return Err("This chat belongs to another workspace.".into());
        }
        return Ok(view);
    }
    let store = state.chat.store.clone();
    let session_window = tauri::async_runtime::spawn_blocking(move || {
        store.get_session_window(&id, before_seq, store::PAGE_EVENTS)
    })
    .await
    .map_err(|error| format!("Could not read the session: {error}"))??
    .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&session_window.workspace) != workspace.as_path() {
        return Err("This chat belongs to another workspace.".into());
    }
    Ok(state.chat.view(session_window))
}

#[tauri::command]
pub async fn session_send(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    text: String,
    attachments: Vec<IncomingAttachment>,
) -> Result<SendResult, String> {
    let window_workspace = workspace_for_window(&state, window.label())?;
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    let store = state.chat.store.clone();
    let load_id = session_id.clone();
    let lookup = store.clone();
    let meta = tauri::async_runtime::spawn_blocking(move || lookup.session_meta(&load_id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&meta.workspace) != window_workspace.as_path() {
        return Err(
            "This chat belongs to another workspace. Open that workspace in its own window.".into(),
        );
    }
    if !Path::new(&meta.workspace).is_dir() {
        return Err(format!("Workspace is unavailable: {}", meta.workspace));
    }
    config.workspace = meta.workspace.clone();

    let text = text.trim().to_string();
    let (message, display, attachment_names) =
        build_user_turn(&text, &attachments, &state.spill_root, &session_id)?;
    let title = if meta.turns == 0 {
        let title = session_title(&text, &attachment_names);
        (!title.is_empty()).then_some(title)
    } else {
        None
    };
    let turn = NewUserTurn {
        message,
        display: Some(display),
        title,
    };

    let (cancel_handle, cancel) = CancelHandle::new();
    let (run_id, view) = start_run(
        &app,
        state.inner(),
        window.label(),
        &session_id,
        config,
        turn,
        cancel,
        cancel_handle,
    )?;
    // A user message ends the wake chain a job notice may have started.
    state.chat.live().wakes.remove(&session_id);

    let session = view
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(SendResult { run_id, session })
}

/// Register a run for an already-built user turn and spawn the agent loop. The
/// caller decides what the turn means: a typed message or a job notice.
#[allow(clippy::too_many_arguments)]
fn start_run(
    app: &AppHandle,
    state: &SharedState,
    window: &str,
    session_id: &str,
    config: crate::AppConfig,
    turn: NewUserTurn,
    cancel: crate::tools::Cancel,
    cancel_handle: CancelHandle,
) -> Result<(String, Arc<Mutex<SessionView>>), String> {
    let store = state.chat.store.clone();
    let run_id = state.next_id();

    // Hold the live lock across the short SQLite write so two clients cannot
    // both start a run for the same session.
    let (view, start_seq, messages) = {
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
        (view, started.start_seq, messages)
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
        ctx.finish_error(messages, format!("The agent stopped unexpectedly: {detail}"))
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

#[tauri::command]
pub fn session_stop(window: WebviewWindow, state: State<'_, SharedState>, session_id: String) {
    if let Some(handle) = state.chat.live().compacting.get(&session_id)
        && handle.window == window.label()
    {
        handle.cancel();
    }
    let run_id = {
        let live = state.chat.live();
        let mut found = None;
        for (id, handle) in live.runs.iter() {
            if handle.session_id == session_id && handle.window == window.label() {
                handle.cancel.cancel();
                found = Some(id.clone());
                break;
            }
        }
        found
    };
    if let Some(run_id) = run_id {
        resolve_approvals(state.inner(), &run_id);
    }
}

/// Announce a settled background job to the session that started it. A busy
/// session gets the notice queued for its next step; an idle one wakes with it,
/// bounded by [`MAX_WAKES`] so a job whose completion starts another job cannot
/// wake the session forever.
pub(crate) fn job_settled(
    app: &AppHandle,
    state: &SharedState,
    job: crate::tools::jobs::JobSettled,
) {
    let (summary, message) = job_notice(&job);
    let notice = JobNotice { summary, message };
    let session_id = job.owner.session_id.clone();
    let window = job.owner.window.clone();
    let chat = &state.chat;

    let wake = {
        let mut live = chat.live();
        let busy = live.runs.values().any(|run| run.session_id == session_id)
            || live.compacting.contains_key(&session_id)
            || live.closing.contains(&window)
            || live
                .unsaved
                .values()
                .any(|(_, end)| end.session_id == session_id);
        let spent = live.wakes.get(&session_id).copied().unwrap_or(0);
        if busy || spent >= MAX_WAKES {
            live.notices
                .entry(session_id.clone())
                .or_default()
                .push(notice.clone());
            false
        } else {
            true
        }
    };
    if !wake {
        return;
    }

    if let Err(error) = wake_notice(app, state, &window, &session_id, notice.clone()) {
        crate::log::key(format_args!("[oaklet] job notice not delivered: {error}"));
        queue_notice(chat, &session_id, notice);
    }
}

/// Open one turn on an idle session so it reads a settled job's result.
fn wake_notice(
    app: &AppHandle,
    state: &SharedState,
    window: &str,
    session_id: &str,
    notice: JobNotice,
) -> Result<(), String> {
    if app.get_webview_window(window).is_none() {
        return Err("The window is closed.".into());
    }
    let Some(meta) = state.chat.store.session_meta(session_id)? else {
        return Err("The session no longer exists.".into());
    };
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    config.workspace = meta.workspace;
    let turn = NewUserTurn {
        message: notice.message,
        display: Some(json!({ "hidden": true })),
        title: None,
    };
    let (cancel_handle, cancel) = CancelHandle::new();
    start_run(
        app,
        state,
        window,
        session_id,
        config,
        turn,
        cancel,
        cancel_handle,
    )?;
    let spent = state
        .chat
        .live()
        .wakes
        .get(session_id)
        .copied()
        .unwrap_or(0);
    state
        .chat
        .live()
        .wakes
        .insert(session_id.to_string(), spent.saturating_add(1));
    if let Err(error) =
        state
            .chat
            .store
            .append_notice(session_id, "muted", &notice.summary, store::now_ms())
    {
        crate::log::key(format_args!(
            "[oaklet] could not record a job notice: {error}"
        ));
    }
    Ok(())
}

fn queue_notice(chat: &ChatState, session_id: &str, notice: JobNotice) {
    chat.live()
        .notices
        .entry(session_id.to_string())
        .or_default()
        .push(notice);
}

/// Deliver notices that settled while the session was mid-run but arrived
/// after its last step: the run's end wakes the session once with them.
fn resume_notices(ctx: &RunContext) {
    let chat = &ctx.state.chat;
    let spent = {
        let live = chat.live();
        if live.notices.get(&ctx.session_id).is_none_or(Vec::is_empty) {
            return;
        }
        live.wakes.get(&ctx.session_id).copied().unwrap_or(0)
    };
    if spent >= MAX_WAKES {
        return;
    }
    let notices = chat.take_notices(&ctx.session_id);
    let summary = notices
        .iter()
        .map(|notice| notice.summary.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let text = notices
        .iter()
        .filter_map(|notice| {
            notice
                .message
                .pointer("/content/0/text")
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let notice = JobNotice {
        summary,
        message: json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
    };
    if let Err(error) = wake_notice(
        ctx.app(),
        &ctx.state,
        &ctx.window_label,
        &ctx.session_id,
        notice.clone(),
    ) {
        crate::log::key(format_args!("[oaklet] job notice not delivered: {error}"));
        queue_notice(chat, &ctx.session_id, notice);
    }
}

/// The model-facing completion text and the one-line transcript summary.
fn job_notice(job: &crate::tools::jobs::JobSettled) -> (String, Value) {
    let line = job.command.lines().next().unwrap_or("").trim();
    let mut label: String = line.chars().take(NOTICE_LABEL_CHARS).collect();
    if label.chars().count() < line.chars().count() {
        label.push('…');
    }
    let origin = if label.is_empty() {
        String::new()
    } else {
        format!(" (bash: {label})")
    };
    let status = match job.exit {
        Some(code) => format!("[status: {}, exit code: {code}]", job.status),
        None => format!("[status: {}]", job.status),
    };
    let summary = format!("background job {}{origin} finished {status}", job.id);
    let body = job.output.trim_end();
    let text = if job.output.chars().count() <= INLINE_NOTICE_CHARS {
        if body.is_empty() {
            format!("{summary}. No new output.")
        } else {
            format!("{summary}. Output:\n{body}")
        }
    } else {
        format!("{summary}. Read its output with job_output.")
    };
    (
        summary,
        json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
    )
}

#[tauri::command]
pub async fn session_compact(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
) -> Result<CompactResult, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    let store = state.chat.store.clone();
    let id = session_id.clone();
    let meta = tauri::async_runtime::spawn_blocking(move || store.session_meta(&id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&meta.workspace) != workspace.as_path() {
        return Err("This chat belongs to another workspace.".into());
    }
    let cancel = {
        let mut live = state.chat.live();
        if live.closing.contains(window.label())
            || live
                .unsaved
                .values()
                .any(|(_, end)| end.session_id == session_id)
        {
            return Err("This window is closing or has an unsaved reply. Retry closing it to save the chat.".into());
        }
        if live.compacting.contains_key(&session_id)
            || live.runs.values().any(|run| run.session_id == session_id)
        {
            return Err("Stop the run before compacting this chat.".into());
        }
        let (handle, cancel) = CancelHandle::new();
        live.compacting.insert(
            session_id.clone(),
            CompactionHandle {
                window: window.label().to_string(),
                cancel: handle,
            },
        );
        cancel
    };

    let slot = CompactionSlot {
        chat: &state.chat,
        session_id: session_id.clone(),
    };
    let result = compact_session(window.app_handle(), state.inner(), &session_id, &cancel).await;
    drop(slot);

    let (session, before, after, compacted) = result?;
    Ok(CompactResult {
        session: state.chat.view(session),
        before,
        after,
        compacted,
    })
}

/// Clears the manual-compaction flag even if the command is dropped mid-way.
struct CompactionSlot<'a> {
    chat: &'a ChatState,
    session_id: String,
}

impl Drop for CompactionSlot<'_> {
    fn drop(&mut self) {
        self.chat.live().compacting.remove(&self.session_id);
    }
}

#[tauri::command]
pub fn session_compact_cancel(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
) {
    if let Some(handle) = state.chat.live().compacting.get(&session_id)
        && handle.window == window.label()
    {
        handle.cancel();
    }
}

async fn compact_session(
    app: &AppHandle,
    state: &SharedState,
    session_id: &str,
    cancel: &crate::tools::Cancel,
) -> Result<(store::SessionWindow, usize, usize, bool), String> {
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.")?
        .clone();
    let store = state.chat.store.clone();
    let id = session_id.to_string();
    let (meta, base, version) = tauri::async_runtime::spawn_blocking(move || {
        let meta = store
            .session_meta(&id)?
            .ok_or("This session no longer exists.")?;
        let (surface, version) = store.context_snapshot(&id)?;
        Ok::<_, String>((meta, trim_incomplete_tool_block(surface), version))
    })
    .await
    .map_err(|error| error.to_string())??;
    config.workspace = meta.workspace;
    if !Path::new(&config.workspace).is_dir() {
        return Err("Workspace is unavailable.".into());
    }
    if base.is_empty() {
        return Ok((load_window(state, session_id).await?, 0, 0, false));
    }
    let mut tools = crate::tools::ToolRegistry::with_host_tools(app, &config, cancel).await;
    let history = crate::tools::history::HistoryRead::new(
        state.chat.store.clone(),
        session_id.into(),
        Arc::new(Mutex::new(Vec::new())),
    );
    if history.has_entries() {
        tools.push(Box::new(history));
    }
    let workspace = config.workspace.clone();
    let (instructions, sources) = tauri::async_runtime::spawn_blocking(move || {
        let root = Path::new(&workspace);
        (
            crate::instructions::WorkspaceInstructions::load(root).render(),
            crate::architecture::prompt_model_sources(root),
        )
    })
    .await
    .map_err(|error| error.to_string())?;
    let mut messages =
        crate::agent::initial_messages(&config, &tools, &sources, instructions.as_deref(), base);
    let before = context::request_tokens(&messages, &tools.schemas());
    let mut changed = false;
    if let Some(next) = compaction::trim_tool_outputs(&messages) {
        // A trim is only staged here, so a failed manual summary leaves the whole operation unchanged.
        messages = next;
        changed = true;
    }
    match compaction::compact(
        &reqwest::Client::new(),
        &config,
        &messages,
        &tools,
        compaction::Trigger::Manual,
        cancel,
    )
    .await?
    {
        Some(next) => {
            messages = next;
        }
        None if !changed => {
            return Ok((load_window(state, session_id).await?, before, before, false));
        }
        None => {}
    }
    let history = crate::tools::history::HistoryRead::new(
        state.chat.store.clone(),
        session_id.into(),
        Arc::new(Mutex::new(Vec::new())),
    );
    if tools.get("history_read").is_none() {
        tools.push(Box::new(history));
    }
    messages = crate::agent::initial_messages(
        &config,
        &tools,
        &sources,
        instructions.as_deref(),
        messages.split_off(1),
    );
    let after = context::request_tokens(&messages, &tools.schemas());
    if after >= before {
        return Err(
            "The replacement and history tool would not shrink the request; history was preserved."
                .into(),
        );
    }
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let surface = messages.split_off(1);
    let store = state.chat.store.clone();
    let id = session_id.to_string();
    let session = tauri::async_runtime::spawn_blocking(move || {
        store.apply_surface(
            &id,
            version,
            &surface,
            (before, after),
            ContextUsage {
                used: after,
                window: config.context_window,
            },
            store::now_ms(),
        )
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok((session, before, after, true))
}

async fn load_window(
    state: &SharedState,
    session_id: &str,
) -> Result<store::SessionWindow, String> {
    let store = state.chat.store.clone();
    let id = session_id.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        store.get_session_window(&id, None, store::PAGE_EVENTS)
    })
    .await
    .map_err(|error| format!("Could not read the session: {error}"))??
    .ok_or_else(|| "This session no longer exists.".to_string())
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

fn build_user_turn(
    text: &str,
    attachments: &[IncomingAttachment],
    spill_root: &Path,
    session_id: &str,
) -> Result<(Value, Value, Vec<String>), String> {
    if text.is_empty() && attachments.is_empty() {
        return Err("A message is required.".into());
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "At most {MAX_ATTACHMENTS} attachments can be sent at once."
        ));
    }

    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }

    let mut sent = Vec::new();
    let mut names = Vec::new();
    for (index, attachment) in attachments.iter().enumerate() {
        match attachment {
            IncomingAttachment::Image {
                name,
                bytes,
                data_url,
                thumbnail,
            } => {
                if data_url.len() > MAX_IMAGE_DATA_URL {
                    return Err(format!("{name} is too large to attach."));
                }
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": data_url },
                }));
                sent.push(SentAttachment::Image {
                    name: name.clone(),
                    bytes: *bytes,
                    thumbnail: thumbnail.clone(),
                });
                names.push(name.clone());
            }
            IncomingAttachment::File {
                name,
                bytes,
                text: file_text,
            } => {
                if file_text.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "{name} is larger than {} KB. Put it in the workspace and mention its path instead.",
                        MAX_FILE_BYTES / 1000
                    ));
                }
                // Save oversized text before trimming it; a failed write keeps
                // the old inline behavior rather than dropping the content.
                let spilled = (file_text.len() > ATTACHMENT_SPILL_BYTES)
                    .then(|| {
                        spill::save(
                            spill_root,
                            session_id,
                            &format!("{}-{name}", index + 1),
                            file_text,
                        )
                    })
                    .flatten();
                let body = match spilled {
                    Some(spilled) => attachment_preview(name, file_text, &spilled),
                    None => format!("Attached file {name}:\n\n{file_text}"),
                };
                parts.push(json!({ "type": "text", "text": body }));
                sent.push(SentAttachment::File {
                    name: name.clone(),
                    bytes: *bytes,
                });
                names.push(name.clone());
            }
        }
    }

    let message = if attachments.is_empty() {
        json!({ "role": "user", "content": text })
    } else {
        json!({ "role": "user", "content": parts })
    };
    let mut display = json!({ "text": text });
    if !sent.is_empty() {
        display["attachments"] = json!(sent);
    }

    Ok((message, display, names))
}

/// Inline preview for a spilled attachment: enough to recognize the file,
/// plus the path for exactly paging through the rest.
fn attachment_preview(name: &str, text: &str, spilled: &spill::Spilled) -> String {
    let preview = spill::preview(
        text,
        ATTACHMENT_HEAD_LINES,
        ATTACHMENT_TAIL_LINES,
        ATTACHMENT_HEAD_CHARS,
        ATTACHMENT_TAIL_CHARS,
    );
    let mut out = format!(
        "Attached file {name}: full text ({} lines, {} bytes) saved to {}. Use read_file with offset and limit to page through it.\n\n",
        spilled.lines,
        spilled.bytes,
        spilled.path.display()
    );
    out.push_str(&preview.head);
    if preview.omitted_lines > 0 || preview.clipped {
        let note = if preview.omitted_lines > 0 {
            format!(
                "[... {} lines omitted; the full text is in the saved file ...]",
                preview.omitted_lines
            )
        } else {
            "[... preview clipped; the full text is in the saved file ...]".to_string()
        };
        out.push_str("\n\n");
        out.push_str(&note);
        out.push_str("\n\n");
    } else {
        // Head and tail meet, so the preview already holds the whole text.
        out.push('\n');
    }
    out.push_str(&preview.tail);
    out
}

/// First user message on one line; attached file names are the fallback.
fn session_title(text: &str, attachment_names: &[String]) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let source = if normalized.is_empty() {
        attachment_names.join(", ")
    } else {
        normalized
    };
    let source = source.trim().to_string();
    if source.chars().count() > 60 {
        let mut title: String = source.chars().take(59).collect();
        title.push('…');
        title
    } else {
        source
    }
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

fn merge_stream(items: &mut Vec<Value>, run_id: &str, kind: &str, text: &str) {
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

fn settle_streaming(items: &mut [Value]) {
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

fn settle_unfinished_tools(items: &mut [Value]) {
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

fn refresh_blocked(view: &mut SessionView) {
    view.blocked = is_blocked(&view.items);
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

/// Close an incomplete tool-call block in model history without truncating
/// later user turns. The database is repaired separately at run finish; this
/// keeps replayed/legacy histories valid before a request.
fn trim_incomplete_tool_block(mut messages: Vec<Value>) -> Vec<Value> {
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

fn find_tool_item<'a>(items: &'a mut [Value], id: &str) -> Option<&'a mut Value> {
    items.iter_mut().rev().find(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item.get("id").and_then(Value::as_str) == Some(id)
    })
}

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
    used: usize,
    window: usize,
}

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
        }
    }

    fn update_view(&self, update: impl FnOnce(&mut SessionView)) {
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut view);
    }

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

    pub fn tool_result(&self, id: &str, ok: bool, output: &str, diffs: &[FileDiff]) -> Value {
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
                }
                found = true;
                break;
            }
            if !found {
                drop(pending);
                self.record(EventPayload::Message {
                    message: json!({ "role": "tool", "tool_call_id": id, "content": output }),
                    display: Some(json!({
                        "runId": self.run_id,
                        "ok": ok,
                        "diffs": diffs,
                    })),
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

    pub fn context(&self, used: usize, window: usize) {
        self.update_view(|view| view.context = Some(ContextUsage { used, window }));
        self.emit(
            "agent:context",
            ContextEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                used,
                window,
            },
        );
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
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::{
        ATTACHMENT_SPILL_BYTES, ChatState, IncomingAttachment, NewItem, RunHandle, SessionView,
        ToolItem, ToolStatus, build_user_turn, close_incomplete_tool_calls, format_tokens,
        is_blocked, merge_stream, project_events, session_title, settle_streaming,
        settle_unfinished_tools, surface_messages, trim_incomplete_tool_block, view_from_session,
    };
    use crate::store::{self, Event, EventPayload};
    use crate::tools::CancelHandle;

    #[tokio::test]
    async fn draining_waits_for_cancelled_compaction_to_settle() {
        let state = ChatState::in_memory();
        state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        let (handle, cancel) = CancelHandle::new();
        state.live().compacting.insert(
            "s1".into(),
            super::CompactionHandle {
                window: "main".into(),
                cancel: handle,
            },
        );
        state.live().compacting.get("s1").unwrap().cancel();
        assert!(cancel.is_cancelled());
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                state.drain(Some("main"))
            )
            .await
            .is_err()
        );
        state.live().compacting.remove("s1");
        state.drain(Some("main")).await.unwrap();
    }

    #[test]
    fn dropping_a_compaction_slot_releases_the_session() {
        let state = ChatState::in_memory();
        let (handle, _cancel) = CancelHandle::new();
        state.live().compacting.insert(
            "s1".into(),
            super::CompactionHandle {
                window: "main".into(),
                cancel: handle,
            },
        );
        let slot = super::CompactionSlot {
            chat: &state,
            session_id: "s1".into(),
        };
        drop(slot);
        assert!(state.live().compacting.is_empty());
    }

    #[test]
    fn injected_store_recovers_stale_runs_and_shares_subsequent_writes() {
        let store = store::Store::in_memory().unwrap();
        store.create_session("shared", "/workspace", 1).unwrap();
        store
            .begin_run(
                "shared",
                "stale",
                &store::NewUserTurn {
                    message: json!({"role":"user","content":"hello"}),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let chat = ChatState::from_store(store.clone()).unwrap();
        let session = store.get_session("shared").unwrap().unwrap();
        assert!(session.events.iter().any(
            |e| matches!(&e.payload,EventPayload::Notice {text,..} if text == super::INTERRUPTED_NOTICE)
        ));
        store.create_session("later", "/workspace", 3).unwrap();
        assert!(chat.store.get_session("later").unwrap().is_some());
        assert_eq!(store.interrupt_stale_runs(4, &json!({})).unwrap(), 0);
    }

    #[tokio::test]
    async fn a_failed_close_retains_the_reply_until_a_retry_can_save_it() {
        let state = ChatState::in_memory();
        let end = Arc::new(store::RunEnd {
            session_id: "s1".into(),
            run_id: "r1".into(),
            start_seq: 1,
            status: "stopped",
            finished_at: 3,
            appended: vec![EventPayload::Message {
                message: json!({ "role": "assistant", "content": "partial answer" }),
                display: None,
            }],
            compaction: None,
            context: None,
            error: None,
        });
        state
            .live()
            .unsaved
            .insert("r1".into(), ("main".into(), end));
        assert!(state.drain(Some("main")).await.is_err());
        assert!(state.live().unsaved.contains_key("r1"));
        state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        state
            .store
            .begin_run(
                "s1",
                "r1",
                &store::NewUserTurn {
                    message: json!({ "role": "user", "content": "hello" }),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        state.drain(Some("main")).await.unwrap();
        assert!(state.live().unsaved.is_empty());
        assert_eq!(
            state.store.get_session("s1").unwrap().unwrap().events.len(),
            2
        );
    }

    #[test]
    fn live_runs_override_the_persisted_view() {
        let state = ChatState::in_memory();
        let session = state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        let view = SessionView {
            id: "s1".into(),
            workspace: "/tmp/ws".into(),
            title: String::new(),
            updated_at: 1,
            turns: 0,
            items: vec![json!({
                "kind": "tool",
                "runId": "r1",
                "id": "t1",
                "name": "run_command",
                "status": "awaiting",
                "summary": "ls",
            })],
            first_seq: None,
            has_more: false,
            context: None,
            running: true,
            blocked: true,
            compacting: false,
            active_run_id: Some("r1".into()),
        };
        let (cancel, _cancel) = CancelHandle::new();
        state.live().runs.insert(
            "r1".into(),
            RunHandle {
                session_id: "s1".into(),
                window: "main".into(),
                cancel,
                view: Arc::new(Mutex::new(view)),
            },
        );

        let live = state.live_view("s1").unwrap();
        assert!(live.running);
        assert_eq!(live.active_run_id.as_deref(), Some("r1"));

        let summaries = state.summaries(vec![store::SessionSummary {
            id: "s1".into(),
            title: String::new(),
            updated_at: 1,
            turns: 0,
        }]);
        assert!(summaries[0].running);
        assert!(summaries[0].blocked);

        let idle = view_from_session(session, false);
        assert!(!idle.running);
    }

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
    fn repairs_pending_events_for_unanswered_tool_calls() {
        let mut pending = vec![
            EventPayload::Message {
                message: json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        { "id": "a", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
                        { "id": "b", "type": "function", "function": { "name": "read_file", "arguments": "{}" } },
                    ],
                }),
                display: Some(json!({ "runId": "r1" })),
            },
            EventPayload::Message {
                message: json!({ "role": "tool", "tool_call_id": "a", "content": "" }),
                display: Some(json!({
                    "runId": "r1",
                    "name": "read_file",
                    "pending": true,
                })),
            },
        ];

        close_incomplete_tool_calls(&mut pending, "r1");
        assert_eq!(pending.len(), 3);
        let EventPayload::Message { message, display } = &pending[1] else {
            panic!("expected a tool message");
        };
        assert!(
            message["content"]
                .as_str()
                .is_some_and(|text| text.contains("unknown"))
        );
        assert_eq!(
            display
                .as_ref()
                .and_then(|display| display.get("pending"))
                .and_then(serde_json::Value::as_bool),
            None
        );
        let EventPayload::Message { message, .. } = &pending[2] else {
            panic!("expected a synthetic tool result");
        };
        assert_eq!(message["tool_call_id"], "b");
    }

    #[test]
    fn reads_the_composer_attachment_shape() {
        let image: IncomingAttachment = serde_json::from_value(json!({
            "kind": "image",
            "name": "shot.png",
            "bytes": 10,
            "dataUrl": "data:image/png;base64,AAA",
            "thumbnail": "data:image/jpeg;base64,BBB",
        }))
        .unwrap();
        match image {
            IncomingAttachment::Image { data_url, .. } => {
                assert_eq!(data_url, "data:image/png;base64,AAA");
            }
            _ => panic!("expected an image"),
        }

        let file: IncomingAttachment = serde_json::from_value(json!({
            "kind": "file",
            "name": "notes.md",
            "bytes": 3,
            "text": "hi",
        }))
        .unwrap();
        match file {
            IncomingAttachment::File { text, .. } => assert_eq!(text, "hi"),
            _ => panic!("expected a file"),
        }
    }

    #[test]
    fn serializes_views_and_items_in_the_frontend_shape() {
        let view = SessionView {
            id: "s1".into(),
            workspace: "/tmp/ws".into(),
            title: "chat".into(),
            updated_at: 7,
            turns: 2,
            items: Vec::new(),
            first_seq: None,
            has_more: false,
            context: None,
            running: false,
            blocked: false,
            compacting: false,
            active_run_id: None,
        };
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["workspace"], "/tmp/ws");
        assert_eq!(value["updatedAt"], 7);
        assert_eq!(value["activeRunId"], serde_json::Value::Null);
        assert_eq!(value["context"], serde_json::Value::Null);
        assert_eq!(value["firstSeq"], serde_json::Value::Null);
        assert_eq!(value["hasMore"], false);

        let item = serde_json::to_value(NewItem::Tool(Box::new(ToolItem {
            run_id: "r1".into(),
            id: "t1".into(),
            name: "read_file".into(),
            status: ToolStatus::Pending,
            summary: "a.ts".into(),
            output: None,
            started_at: Some(5),
            duration_ms: None,
            request_id: None,
            approval_prompt: None,
            approval_detail: None,
            diffs: Vec::new(),
        })))
        .unwrap();
        assert_eq!(item["kind"], "tool");
        assert_eq!(item["runId"], "r1");
        assert_eq!(item["startedAt"], 5);
        assert!(item.get("output").is_none());
    }

    #[test]
    fn builds_user_content_and_display_items_for_attachments() {
        let (message, display, names) = build_user_turn(
            "look",
            &[
                IncomingAttachment::Image {
                    name: "shot.png".into(),
                    bytes: 10,
                    data_url: "data:image/png;base64,AAA".into(),
                    thumbnail: "data:image/jpeg;base64,BBB".into(),
                },
                IncomingAttachment::File {
                    name: "notes.md".into(),
                    bytes: 3,
                    text: "hi".into(),
                },
            ],
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "run-1",
        )
        .unwrap();

        assert_eq!(names, vec!["shot.png", "notes.md"]);
        assert_eq!(
            message["content"][0],
            json!({ "type": "text", "text": "look" })
        );
        assert_eq!(
            message["content"][1],
            json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } })
        );
        assert_eq!(
            message["content"][2],
            json!({ "type": "text", "text": "Attached file notes.md:\n\nhi" })
        );
        assert_eq!(display["text"], "look");
        assert_eq!(
            display["attachments"][0]["thumbnail"],
            "data:image/jpeg;base64,BBB"
        );
        assert_eq!(display["attachments"][1]["name"], "notes.md");
    }

    #[test]
    fn plain_text_keeps_the_string_content_shape() {
        let (message, display, names) =
            build_user_turn("hello", &[], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").unwrap();
        assert_eq!(message["content"], "hello");
        assert_eq!(display["text"], "hello");
        assert!(names.is_empty());
        assert!(build_user_turn("", &[], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").is_err());
    }

    #[test]
    fn rejects_oversized_attachments() {
        let file = IncomingAttachment::File {
            name: "big.txt".into(),
            bytes: 1,
            text: "x".repeat(200_001),
        };
        assert!(
            build_user_turn("", &[file], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").is_err()
        );
    }

    #[test]
    fn spills_oversized_text_attachments_with_a_preview() {
        let spill_root = crate::tools::test_support::temp_root("attachment-spill");
        let head: Vec<String> = (1..=100).map(|line| format!("line {line}")).collect();
        let tail: Vec<String> = (500..=540).map(|line| format!("line {line}")).collect();
        let text = format!(
            "{}\n{}\n{}",
            head.join("\n"),
            "x".repeat(ATTACHMENT_SPILL_BYTES),
            tail.join("\n")
        );
        let file = IncomingAttachment::File {
            name: "notes.md".into(),
            bytes: text.len() as u64,
            text: text.clone(),
        };
        let (message, _display, names) =
            build_user_turn("", &[file], &spill_root, "run-1").unwrap();

        let directory = std::fs::read_dir(&spill_root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = directory.join("1-notes.md.txt");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "spill file keeps the complete attachment"
        );
        let body = message["content"][0]["text"].as_str().unwrap();
        assert_eq!(names, vec!["notes.md"]);
        assert!(body.contains(&path.display().to_string()));
        assert!(body.contains(&format!("{} lines", text.lines().count())));
        assert!(body.contains(&format!("{} bytes", text.len())));
        assert!(body.contains("line 1"));
        assert!(body.contains("line 540"));
        assert!(body.contains("lines omitted"));
        assert!(!body.contains(&"x".repeat(1_000)));
        assert!(body.chars().count() < 10_000);
        std::fs::remove_dir_all(&spill_root).ok();
    }

    #[test]
    fn attachment_spill_failure_keeps_the_inline_text() {
        let root = crate::tools::test_support::temp_root("attachment-spill-fail");
        let spill_root = root.join("spill");
        std::fs::write(&spill_root, "not a directory").unwrap();
        let text = "x".repeat(ATTACHMENT_SPILL_BYTES + 1);
        let file = IncomingAttachment::File {
            name: "notes.txt".into(),
            bytes: text.len() as u64,
            text: text.clone(),
        };
        let (message, _item, _names) = build_user_turn("", &[file], &spill_root, "run-1").unwrap();

        assert_eq!(
            message["content"][0]["text"],
            format!("Attached file notes.txt:\n\n{text}")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn derives_titles_from_text_or_attachment_names() {
        assert_eq!(
            session_title("  Fix\n the   parser ", &[]),
            "Fix the parser"
        );
        assert_eq!(
            session_title("", &["error.log".into(), "notes.md".into()]),
            "error.log, notes.md"
        );
        let long = "x".repeat(80);
        assert_eq!(session_title(&long, &[]).chars().count(), 60);
        let cjk = "你".repeat(80);
        assert_eq!(session_title(&cjk, &[]).chars().count(), 60);
        assert_eq!(session_title("", &[]), "");
    }

    #[test]
    fn formats_tokens_like_the_composer() {
        assert_eq!(format_tokens(900), "900");
        assert_eq!(format_tokens(12_288), "12k");
        assert_eq!(format_tokens(1_536), "1.5k");
    }

    #[test]
    fn merges_streams_and_settles_on_a_kind_boundary() {
        let mut items = Vec::new();
        merge_stream(&mut items, "r1", "reasoning", "think");
        let started_at = items[0]["startedAt"].clone();
        merge_stream(&mut items, "r1", "reasoning", "ing");
        assert_eq!(items[0]["startedAt"], started_at);
        merge_stream(&mut items, "r1", "assistant", "hello");
        merge_stream(&mut items, "r1", "assistant", " world");
        merge_stream(&mut items, "r1", "reasoning", "again");

        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["text"], "thinking");
        assert_eq!(items[0]["streaming"], false);
        assert!(items[0]["durationMs"].as_u64().is_some());
        assert!(items[0].get("startedAt").is_none());
        assert_eq!(items[1]["text"], "hello world");
        assert_eq!(items[1]["streaming"], false);
        assert_eq!(items[2]["streaming"], true);
        assert!(items[2]["startedAt"].as_u64().is_some());
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
    fn settles_unfinished_tools_at_the_end_of_a_run() {
        let mut items = vec![
            json!({
                "kind": "tool",
                "id": "a",
                "status": "awaiting",
                "requestId": "1",
                "diffs": [{ "path": "a.txt", "oldText": null, "newText": "one" }],
            }),
            json!({ "kind": "tool", "id": "b", "status": "ok", "output": "done" }),
            json!({ "kind": "assistant", "runId": "r", "text": "hi", "streaming": true }),
        ];
        settle_streaming(&mut items);
        settle_unfinished_tools(&mut items);

        assert_eq!(items[0]["status"], "error");
        assert!(items[0].get("requestId").is_none());
        assert!(items[0].get("diffs").is_none());
        assert_eq!(items[1]["status"], "ok");
        assert_eq!(items[2]["streaming"], false);
        assert!(!is_blocked(&items));
    }

    #[test]
    fn projects_events_into_items_without_leaking_display_data() {
        let events = vec![
            Event {
                seq: 1,
                created_at: 1,
                payload: EventPayload::Message {
                    message: json!({
                        "role": "user",
                        "content": [
                            { "type": "text", "text": "look" },
                            { "type": "image_url", "image_url": { "url": "data:full" } },
                        ],
                    }),
                    display: Some(json!({
                        "text": "look",
                        "attachments": [{
                            "kind": "image",
                            "name": "shot.png",
                            "bytes": 10,
                            "thumbnail": "data:thumb",
                        }],
                    })),
                },
            },
            Event {
                seq: 2,
                created_at: 2,
                payload: EventPayload::Message {
                    message: json!({
                        "role": "assistant",
                        "content": "done",
                        "reasoning_content": "thinking",
                        "tool_calls": [{
                            "id": "c1",
                            "type": "function",
                            "function": { "name": "read_file", "arguments": "{}" },
                        }],
                    }),
                    display: Some(json!({ "runId": "r1", "reasoningDurationMs": 1250 })),
                },
            },
            Event {
                seq: 3,
                created_at: 3,
                payload: EventPayload::Message {
                    message: json!({ "role": "tool", "tool_call_id": "c1", "content": "file body" }),
                    display: Some(json!({
                        "runId": "r1",
                        "name": "read_file",
                        "summary": "a.rs",
                        "ok": false,
                        "diffs": [{ "path": "a.rs", "oldText": null, "newText": "x" }],
                    })),
                },
            },
            Event {
                seq: 4,
                created_at: 4,
                payload: EventPayload::Notice {
                    tone: "muted".into(),
                    text: "Stopped.".into(),
                },
            },
            Event {
                seq: 5,
                created_at: 5,
                payload: EventPayload::Message {
                    message: json!({ "role": "user", "content": "hidden" }),
                    display: Some(json!({ "hidden": true })),
                },
            },
        ];

        let items = project_events(&events);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0]["kind"], "user");
        assert_eq!(items[0]["text"], "look");
        assert_eq!(items[0]["attachments"][0]["thumbnail"], "data:thumb");
        assert_eq!(items[1]["kind"], "reasoning");
        assert_eq!(items[1]["durationMs"], 1250);
        assert_eq!(items[2]["kind"], "assistant");
        assert_eq!(items[2]["runId"], "r1");
        assert_eq!(items[3]["kind"], "tool");
        assert_eq!(items[3]["status"], "error");
        assert_eq!(items[3]["summary"], "a.rs");
        assert_eq!(items[3]["diffs"][0]["path"], "a.rs");
        assert_eq!(items[4]["kind"], "notice");

        let model = surface_messages(&events);
        assert_eq!(model.len(), 4);
        assert!(model.iter().all(|message| message.get("oaklet").is_none()));
    }

    #[test]
    fn a_user_message_that_quotes_a_checkpoint_marker_stays_visible() {
        let events = vec![Event {
            seq: 1,
            created_at: 1,
            payload: EventPayload::Message {
                message: json!({
                    "role": "user",
                    "content": "the log shows <compacted-summary> and then stops",
                }),
                display: None,
            },
        }];
        let items = project_events(&events);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], "user");
        assert_eq!(
            items[0]["text"],
            "the log shows <compacted-summary> and then stops"
        );
    }

    #[test]
    fn a_finished_run_projects_back_to_the_same_rows() {
        let state = ChatState::in_memory();
        state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        let message = json!({ "role": "user", "content": "hi" });
        state
            .store
            .begin_run(
                "s1",
                "r1",
                &store::NewUserTurn {
                    message: message.clone(),
                    display: Some(json!({ "text": "hi" })),
                    title: Some("hi".into()),
                },
                2,
            )
            .unwrap();
        let saved = state
            .store
            .finish_run(&store::RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![
                    EventPayload::Message {
                        message: json!({ "role": "assistant", "content": "hello" }),
                        display: Some(json!({ "runId": "r1" })),
                    },
                    EventPayload::Message {
                        message: json!({
                            "role": "tool",
                            "tool_call_id": "c1",
                            "content": "output",
                        }),
                        display: Some(json!({
                            "runId": "r1",
                            "name": "run_command",
                            "summary": "ls",
                            "ok": true,
                            "durationMs": 1250,
                        })),
                    },
                ],
                compaction: None,
                context: Some(store::ContextUsage {
                    used: 10,
                    window: 100,
                }),
                error: None,
            })
            .unwrap();

        let items = project_events(&saved.events);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["kind"], "user");
        assert_eq!(items[1]["kind"], "assistant");
        assert_eq!(items[2]["kind"], "tool");
        assert_eq!(items[2]["status"], "ok");
        assert_eq!(items[2]["output"], "output");
        assert_eq!(items[2]["durationMs"], 1250);
        assert!(
            surface_messages(&saved.events)[2]
                .get("durationMs")
                .is_none()
        );
    }

    #[test]
    fn a_second_turn_rebuilds_the_model_history_from_the_events() {
        let state = ChatState::in_memory();
        state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        let first = json!({ "role": "user", "content": "one" });
        state
            .store
            .begin_run(
                "s1",
                "r1",
                &store::NewUserTurn {
                    message: first,
                    display: Some(json!({ "text": "one" })),
                    title: Some("one".into()),
                },
                2,
            )
            .unwrap();
        state
            .store
            .finish_run(&store::RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "first" }),
                    display: None,
                }],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();

        let second = json!({ "role": "user", "content": "two" });
        let session = state
            .store
            .begin_run(
                "s1",
                "r2",
                &store::NewUserTurn {
                    message: second,
                    display: Some(json!({ "text": "two" })),
                    title: None,
                },
                4,
            )
            .unwrap();

        let model = surface_messages(&session.window.events);
        assert_eq!(model.len(), 3);
        assert_eq!(model[0]["content"], "one");
        assert_eq!(model[1]["content"], "first");
        assert_eq!(model[2]["content"], "two");
    }

    #[test]
    fn tool_trims_change_model_history_without_changing_the_transcript() {
        let user = json!({"role":"user", "content":"inspect"});
        let call = json!({"role":"assistant", "content":"", "tool_calls":[{"id":"call-1", "type":"function", "function":{"name":"read_file", "arguments":"{}"}}]});
        let original =
            json!({"role":"tool", "tool_call_id":"call-1", "content":"full original output"});
        let trimmed = json!({"role":"tool", "tool_call_id":"call-1", "content":"trimmed output"});
        let mut events: Vec<Event> = [user.clone(), call.clone(), original]
            .into_iter()
            .enumerate()
            .map(|(index, message)| Event {
                seq: index as i64 + 1,
                payload: EventPayload::Message {
                    message,
                    display: None,
                },
                created_at: 1,
            })
            .collect();
        let transcript = project_events(&events);
        events.push(Event {
            seq: 4,
            payload: EventPayload::ToolTrim {
                original_seq: 3,
                message: trimmed.clone(),
            },
            created_at: 2,
        });
        assert_eq!(
            surface_messages(&events),
            vec![user.clone(), call.clone(), trimmed]
        );
        assert_eq!(project_events(&events), transcript);
        let smaller = json!({"role":"tool", "tool_call_id":"call-1", "content":"smaller"});
        events.push(Event {
            seq: 5,
            payload: EventPayload::ToolTrim {
                original_seq: 3,
                message: smaller.clone(),
            },
            created_at: 3,
        });
        assert_eq!(surface_messages(&events), vec![user, call, smaller]);
        assert_eq!(project_events(&events), transcript);
    }

    #[test]
    fn a_compaction_shadows_history_but_keeps_the_rendered_transcript() {
        let state = ChatState::in_memory();
        state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        state
            .store
            .begin_run(
                "s1",
                "r1",
                &store::NewUserTurn {
                    message: json!({ "role": "user", "content": "old" }),
                    display: Some(json!({ "text": "old" })),
                    title: Some("old".into()),
                },
                2,
            )
            .unwrap();
        state
            .store
            .finish_run(&store::RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: 1,
                status: "done",
                finished_at: 3,
                appended: vec![EventPayload::Message {
                    message: json!({ "role": "assistant", "content": "old answer" }),
                    display: None,
                }],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        state
            .store
            .begin_run(
                "s1",
                "r2",
                &store::NewUserTurn {
                    message: json!({ "role": "user", "content": "new" }),
                    display: Some(json!({ "text": "new" })),
                    title: None,
                },
                4,
            )
            .unwrap();
        let session = state
            .store
            .apply_compaction(
                "s1",
                &store::CompactionRecord {
                    summary: json!({
                        "role": "user",
                        "content": "<compacted-summary>old turns</compacted-summary>",
                    }),
                    shadowed_through: 2,
                    before: 100,
                    after: 10,
                    context: store::ContextUsage {
                        used: 10,
                        window: 100,
                    },
                },
                5,
            )
            .unwrap();

        let model = surface_messages(&session.events);
        assert_eq!(model.len(), 2);
        assert_eq!(
            model[0]["content"],
            "<compacted-summary>old turns</compacted-summary>"
        );
        assert_eq!(model[1]["content"], "new");

        let items = project_events(&session.events);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0]["kind"], "user");
        assert_eq!(items[1]["kind"], "assistant");
        assert_eq!(items[2]["kind"], "user");
        assert_eq!(items[3]["kind"], "notice");

        assert_eq!(state.store.history_count("s1").unwrap(), 2);
        let (history, total) = state.store.read_history("s1", 1, 10).unwrap();
        assert_eq!(total, 2);
        assert_eq!(history[0].1["content"], "old");
        assert_eq!(history[1].1["content"], "old answer");
        let _ = session;
    }
}

#[cfg(test)]
mod checkpoint_text_regressions {
    use super::*;

    #[test]
    fn leading_checkpoint_text_stays_visible_in_strings_and_content_parts() {
        for content in [
            json!("<compacted-summary>quoted log</compacted-summary>"),
            json!([{"type":"text", "text":"<compacted-summary>quoted log</compacted-summary>"}]),
            json!("The earlier part of this conversation was dropped; explain this message"),
        ] {
            let message = json!({"role":"user", "content":content});
            assert!(!store::is_checkpoint(&message));
            let events = vec![Event {
                seq: 1,
                created_at: 0,
                payload: EventPayload::Message {
                    message: message.clone(),
                    display: None,
                },
            }];
            let items = project_events(&events);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["kind"], "user");
            assert_eq!(surface_messages(&events), [message]);
        }
    }
}

#[cfg(test)]
mod job_notice_tests {
    use super::{ChatState, JobNotice, job_notice};
    use crate::tools::jobs::{JobOwner, JobSettled};
    use serde_json::json;

    fn settled(output: &str) -> JobSettled {
        JobSettled {
            id: "job-1a1ec825407-2".into(),
            owner: JobOwner {
                session_id: "s1".into(),
                window: "main".into(),
            },
            command: "cargo test".into(),
            status: "succeeded",
            exit: Some(0),
            output: output.into(),
        }
    }

    #[test]
    fn small_output_rides_along_and_large_output_points_at_job_output() {
        let (summary, message) = job_notice(&settled("ok\n"));
        assert_eq!(
            summary,
            "background job job-1a1ec825407-2 (bash: cargo test) finished \
             [status: succeeded, exit code: 0]"
        );
        assert!(
            message["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("ok")
        );

        let (_, message) = job_notice(&settled(&"x".repeat(super::INLINE_NOTICE_CHARS + 1)));
        let text = message["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("job_output"));
        assert!(!text.contains("xxx"));
    }

    #[test]
    fn queued_notices_are_taken_once() {
        let state = ChatState::in_memory();
        state
            .live()
            .notices
            .entry("s1".into())
            .or_default()
            .push(JobNotice {
                summary: "done".into(),
                message: json!({}),
            });
        assert_eq!(state.take_notices("s1").len(), 1);
        assert!(state.take_notices("s1").is_empty());
    }
}
