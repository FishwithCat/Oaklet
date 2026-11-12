//! Sessions and runs: the backend owns the conversation, the UI renders it.
//!
//! Commands in this module are the only way the frontend reads or changes chat
//! state. A live run keeps its authoritative [`SessionView`] in memory, so a
//! second client can hydrate through `session_get` while tokens are streaming;
//! the database is written once when the run ends.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, State};

use crate::store::{self, ContextUsage, ImportEntry, NewUserTurn, RunEnd, Session, Store};
use crate::tools::CancelHandle;
use crate::{AppState, SharedState, compaction, context, spill};

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

/// What the UI needs to render one conversation. Model-facing `messages` stay
/// in Rust so long base64 image history never crosses IPC on every turn.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub turns: usize,
    pub items: Vec<Value>,
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
    User {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<SentAttachment>>,
    },
    Tool(ToolItem),
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
    compacting: HashSet<String>,
}

struct RunHandle {
    session_id: String,
    cancel: CancelHandle,
    view: Arc<Mutex<SessionView>>,
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
        let store = Store::open(path)?;
        let recovered = store.interrupt_stale_runs(
            store::now_ms(),
            &json!({ "kind": "notice", "tone": Tone::Error, "text": INTERRUPTED_NOTICE }),
        )?;
        if recovered > 0 {
            eprintln!("[oaklet] marked {recovered} interrupted run(s)");
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

    fn view(&self, session: Session) -> SessionView {
        let compacting = self.live().compacting.contains(&session.id);
        view_from_session(session, compacting)
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
                let compacting = live.compacting.contains(&row.id);
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
    SessionView {
        id: session.id,
        title: session.title,
        updated_at: session.updated_at,
        turns: session.turns,
        items: session.items,
        context: session.context,
        running: false,
        blocked: false,
        compacting,
        active_run_id: None,
    }
}

#[tauri::command]
pub async fn session_create(
    state: State<'_, SharedState>,
    workspace: String,
) -> Result<SessionView, String> {
    let store = state.chat.store.clone();
    let id = state.next_id();
    let now = store::now_ms();
    let session =
        tauri::async_runtime::spawn_blocking(move || store.create_session(&id, &workspace, now))
            .await
            .map_err(|error| format!("Could not create the session: {error}"))??;
    Ok(state.chat.view(session))
}

#[tauri::command]
pub async fn session_list(
    state: State<'_, SharedState>,
    workspace: String,
) -> Result<Vec<SessionSummary>, String> {
    let store = state.chat.store.clone();
    let rows = tauri::async_runtime::spawn_blocking(move || store.list_sessions(&workspace))
        .await
        .map_err(|error| format!("Could not read session history: {error}"))??;
    Ok(state.chat.summaries(rows))
}

#[tauri::command]
pub async fn session_get(state: State<'_, SharedState>, id: String) -> Result<SessionView, String> {
    if let Some(view) = state.chat.live_view(&id) {
        return Ok(view);
    }
    let store = state.chat.store.clone();
    let session = tauri::async_runtime::spawn_blocking(move || store.get_session(&id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    Ok(state.chat.view(session))
}

#[tauri::command]
pub async fn session_send(
    app: AppHandle,
    state: State<'_, SharedState>,
    session_id: String,
    text: String,
    attachments: Vec<IncomingAttachment>,
) -> Result<SendResult, String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    let store = state.chat.store.clone();
    let load_id = session_id.clone();
    let lookup = store.clone();
    let session = tauri::async_runtime::spawn_blocking(move || lookup.get_session(&load_id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if session.workspace != config.workspace {
        return Err("Switch back to this chat's workspace to continue it.".into());
    }

    let text = text.trim().to_string();
    let run_id = state.next_id();
    let (message, item, attachment_names) =
        build_user_turn(&text, &attachments, &config.workspace, &run_id)?;
    let title = if session.turns == 0 {
        let title = session_title(&text, &attachment_names);
        (!title.is_empty()).then_some(title)
    } else {
        None
    };
    let turn = NewUserTurn {
        message,
        item,
        title,
    };

    let (cancel_handle, cancel) = CancelHandle::new();

    // Hold the live lock across the short SQLite write so two clients cannot
    // both start a run for the same session.
    let (view, messages) = {
        let mut live = state.chat.live();
        if live.compacting.contains(&session_id)
            || live.runs.values().any(|run| run.session_id == session_id)
        {
            return Err("This chat is already working.".into());
        }
        let updated = store.begin_run(&session_id, &run_id, &turn, store::now_ms())?;
        let view = SessionView {
            id: updated.id,
            title: updated.title,
            updated_at: updated.updated_at,
            turns: updated.turns,
            items: updated.items,
            context: updated.context,
            running: true,
            blocked: false,
            compacting: false,
            active_run_id: Some(run_id.clone()),
        };
        let messages = updated.messages;
        let view = Arc::new(Mutex::new(view));
        live.runs.insert(
            run_id.clone(),
            RunHandle {
                session_id: session_id.clone(),
                cancel: cancel_handle,
                view: view.clone(),
            },
        );
        (view, messages)
    };

    let ctx = RunContext {
        app: app.clone(),
        state: state.inner().clone(),
        session_id,
        run_id: run_id.clone(),
        view: view.clone(),
        finished: Arc::new(AtomicBool::new(false)),
    };
    tauri::async_runtime::spawn(async move {
        crate::agent::run(config, messages, cancel, ctx).await;
    });

    let session = view
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(SendResult { run_id, session })
}

#[tauri::command]
pub fn session_stop(state: State<'_, SharedState>, session_id: String) {
    let run_id = {
        let live = state.chat.live();
        let mut found = None;
        for (id, handle) in live.runs.iter() {
            if handle.session_id == session_id {
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

#[tauri::command]
pub async fn session_compact(
    state: State<'_, SharedState>,
    session_id: String,
) -> Result<CompactResult, String> {
    {
        let mut live = state.chat.live();
        if live.compacting.contains(&session_id)
            || live.runs.values().any(|run| run.session_id == session_id)
        {
            return Err("Stop the run before compacting this chat.".into());
        }
        live.compacting.insert(session_id.clone());
    }

    let result = compact_session(state.inner(), &session_id).await;

    {
        let mut live = state.chat.live();
        live.compacting.remove(&session_id);
    }

    let (session, before, after, compacted) = result?;
    Ok(CompactResult {
        session: state.chat.view(session),
        before,
        after,
        compacted,
    })
}

async fn compact_session(
    state: &SharedState,
    session_id: &str,
) -> Result<(Session, usize, usize, bool), String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    let store = state.chat.store.clone();
    let id = session_id.to_string();
    let session = tauri::async_runtime::spawn_blocking(move || store.get_session(&id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;

    if session.messages.is_empty() {
        return Ok((session, 0, 0, false));
    }

    let before = context::estimate_tokens(&session.messages);
    let (_handle, cancel) = CancelHandle::new();
    let next =
        compaction::compact_messages(&reqwest::Client::new(), &config, &session.messages, &cancel)
            .await?;
    let Some(messages) = next else {
        return Ok((session, before, before, false));
    };

    let after = context::estimate_tokens(&messages);
    let notice = json!({
        "kind": "notice",
        "tone": Tone::Muted,
        "text": format!(
            "Compacted context: {} → {} tokens.",
            format_tokens(before),
            format_tokens(after)
        ),
    });
    let store = state.chat.store.clone();
    let id = session_id.to_string();
    let compacted = tauri::async_runtime::spawn_blocking(move || {
        store.apply_compaction(
            &id,
            &messages,
            &notice,
            ContextUsage {
                used: after,
                window: config.context_window,
            },
            store::now_ms(),
        )
    })
    .await
    .map_err(|error| format!("Could not save the compacted context: {error}"))??;
    Ok((compacted, before, after, true))
}

#[tauri::command]
pub async fn session_import(
    state: State<'_, SharedState>,
    entries: Vec<ImportEntry>,
) -> Result<usize, String> {
    let store = state.chat.store.clone();
    tauri::async_runtime::spawn_blocking(move || store.import_sessions(&entries))
        .await
        .map_err(|error| format!("Could not import old sessions: {error}"))?
}

/// Cancel every live run, for a workspace switch or app exit.
pub fn stop_all(state: &AppState) {
    let run_ids: Vec<String> = {
        let live = state.chat.live();
        live.runs
            .iter()
            .map(|(run_id, handle)| {
                handle.cancel.cancel();
                run_id.clone()
            })
            .collect()
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
    workspace: &str,
    run_id: &str,
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
                            Path::new(workspace),
                            run_id,
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
    let item = serde_json::to_value(NewItem::User {
        text: text.to_string(),
        attachments: (!sent.is_empty()).then_some(sent),
    })
    .map_err(|error| format!("Could not prepare the message: {error}"))?;

    Ok((message, item, names))
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
        spilled.relative.display()
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
}

fn settle_streaming(items: &mut [Value]) {
    for item in items.iter_mut() {
        let kind = item.get("kind").and_then(Value::as_str);
        if (kind == Some("assistant") || kind == Some("reasoning"))
            && item.get("streaming").and_then(Value::as_bool) == Some(true)
        {
            item["streaming"] = json!(false);
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

/// A stopped or failed run can end between one assistant tool-call message and
/// its results. Drop that trailing block so the saved history is still a valid
/// request for the next turn.
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
    let answered = messages[index + 1..]
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .count();
    if answered < expected {
        messages.truncate(index);
    }
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
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultEvent {
    session_id: String,
    run_id: String,
    id: String,
    ok: bool,
    output: String,
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
    app: AppHandle,
    state: SharedState,
    session_id: String,
    run_id: String,
    view: Arc<Mutex<SessionView>>,
    finished: Arc<AtomicBool>,
}

impl RunContext {
    pub fn app(&self) -> &AppHandle {
        &self.app
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn emit<T: Serialize + Clone>(&self, event: &str, payload: T) {
        let _ = self.app.emit(event, payload);
    }

    fn update_view(&self, update: impl FnOnce(&mut SessionView)) {
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut view);
    }

    pub fn delta(&self, text: &str) {
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

    pub fn tool_start(&self, id: &str, name: &str, summary: &str) {
        let started_at = store::now_ms() as u64;
        self.update_view(|view| {
            settle_streaming(&mut view.items);
            let item = NewItem::Tool(ToolItem {
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
            });
            match serde_json::to_value(item) {
                Ok(item) => view.items.push(item),
                Err(error) => eprintln!("[oaklet] could not record a tool call: {error}"),
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
            },
        );
    }

    pub fn tool_result(&self, id: &str, ok: bool, output: &str) {
        let finished_at = store::now_ms() as u64;
        self.update_view(|view| {
            if let Some(item) = find_tool_item(&mut view.items, id) {
                let status = if ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Error
                };
                item["status"] = json!(status);
                item["output"] = json!(output);
                if let Some(started_at) = item.get("startedAt").and_then(Value::as_u64) {
                    item["durationMs"] = json!(finished_at.saturating_sub(started_at));
                }
                if let Some(object) = item.as_object_mut() {
                    object.remove("startedAt");
                }
            }
            refresh_blocked(view);
        });
        self.emit(
            "agent:tool_result",
            ToolResultEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                id: id.to_string(),
                ok,
                output: output.to_string(),
            },
        );
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
    async fn finish(&self, messages: Vec<Value>, status: &'static str, error: Option<String>) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let messages = trim_incomplete_tool_block(messages);

        let final_view = {
            let mut view = self
                .view
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            settle_streaming(&mut view.items);
            settle_unfinished_tools(&mut view.items);
            match status {
                "error" => {
                    let text = error.clone().unwrap_or_else(|| "The run failed.".into());
                    view.items
                        .push(json!({ "kind": "notice", "tone": Tone::Error, "text": text }));
                }
                "stopped" => view
                    .items
                    .push(json!({ "kind": "notice", "tone": Tone::Muted, "text": "Stopped." })),
                _ => {}
            }
            view.running = false;
            view.active_run_id = None;
            view.blocked = false;
            view.clone()
        };

        let end = RunEnd {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            status,
            finished_at: store::now_ms(),
            messages,
            items: final_view.items.clone(),
            context: final_view.context,
            error: error.clone(),
        };
        let store = self.state.chat.store.clone();
        let saved = tauri::async_runtime::spawn_blocking(move || store.finish_run(&end)).await;

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
            live.runs.remove(&self.run_id);
        }

        if let Some(save_error) = &save_error {
            eprintln!("[oaklet] {save_error}");
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
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::{
        ATTACHMENT_SPILL_BYTES, ChatState, IncomingAttachment, NewItem, RunHandle, SessionView,
        ToolItem, ToolStatus, build_user_turn, format_tokens, is_blocked, merge_stream,
        session_title, settle_streaming, settle_unfinished_tools, trim_incomplete_tool_block,
    };
    use crate::store;
    use crate::tools::CancelHandle;

    #[test]
    fn live_runs_override_the_persisted_view() {
        let state = ChatState::in_memory();
        let session = state.store.create_session("s1", "/tmp/ws", 1).unwrap();
        let view = SessionView {
            id: "s1".into(),
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

        let idle = state.view(session);
        assert!(!idle.running);
    }

    #[test]
    fn drops_a_tool_block_that_stopped_before_its_results() {
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
        let trimmed = trim_incomplete_tool_block(partial);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0]["role"], "user");

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
            title: "chat".into(),
            updated_at: 7,
            turns: 2,
            items: Vec::new(),
            context: None,
            running: false,
            blocked: false,
            compacting: false,
            active_run_id: None,
        };
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["updatedAt"], 7);
        assert_eq!(value["activeRunId"], serde_json::Value::Null);
        assert_eq!(value["context"], serde_json::Value::Null);

        let item = serde_json::to_value(NewItem::Tool(ToolItem {
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
        }))
        .unwrap();
        assert_eq!(item["kind"], "tool");
        assert_eq!(item["runId"], "r1");
        assert_eq!(item["startedAt"], 5);
        assert!(item.get("output").is_none());

        let user = serde_json::to_value(NewItem::User {
            text: "hi".into(),
            attachments: None,
        })
        .unwrap();
        assert_eq!(user, json!({ "kind": "user", "text": "hi" }));
    }

    #[test]
    fn builds_user_content_and_display_items_for_attachments() {
        let (message, item, names) = build_user_turn(
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
            env!("CARGO_MANIFEST_DIR"),
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
        assert_eq!(item["kind"], "user");
        assert_eq!(
            item["attachments"][0]["thumbnail"],
            "data:image/jpeg;base64,BBB"
        );
        assert_eq!(item["attachments"][1]["name"], "notes.md");
    }

    #[test]
    fn plain_text_keeps_the_string_content_shape() {
        let (message, item, names) =
            build_user_turn("hello", &[], env!("CARGO_MANIFEST_DIR"), "run-1").unwrap();
        assert_eq!(message["content"], "hello");
        assert!(item.get("attachments").is_none());
        assert!(names.is_empty());
        assert!(build_user_turn("", &[], env!("CARGO_MANIFEST_DIR"), "run-1").is_err());
    }

    #[test]
    fn rejects_oversized_attachments() {
        let file = IncomingAttachment::File {
            name: "big.txt".into(),
            bytes: 1,
            text: "x".repeat(200_001),
        };
        assert!(build_user_turn("", &[file], env!("CARGO_MANIFEST_DIR"), "run-1").is_err());
    }

    #[test]
    fn spills_oversized_text_attachments_with_a_preview() {
        let root = crate::tools::test_support::temp_root("attachment-spill");
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
        let (message, _item, names) =
            build_user_turn("", &[file], root.to_str().unwrap(), "run-1").unwrap();

        let relative = Path::new(".oaklet/spill/run-1/1-notes.md.txt");
        assert_eq!(
            std::fs::read_to_string(root.join(relative)).unwrap(),
            text,
            "spill file keeps the complete attachment"
        );
        let body = message["content"][0]["text"].as_str().unwrap();
        assert_eq!(names, vec!["notes.md"]);
        assert!(body.contains(&relative.display().to_string()));
        assert!(body.contains(&format!("{} lines", text.lines().count())));
        assert!(body.contains(&format!("{} bytes", text.len())));
        assert!(body.contains("line 1"));
        assert!(body.contains("line 540"));
        assert!(body.contains("lines omitted"));
        assert!(!body.contains(&"x".repeat(1_000)));
        assert!(body.chars().count() < 10_000);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn attachment_spill_failure_keeps_the_inline_text() {
        let root = crate::tools::test_support::temp_root("attachment-spill-fail");
        std::fs::write(root.join(".oaklet"), "not a directory").unwrap();
        let text = "x".repeat(ATTACHMENT_SPILL_BYTES + 1);
        let file = IncomingAttachment::File {
            name: "notes.txt".into(),
            bytes: text.len() as u64,
            text: text.clone(),
        };
        let (message, _item, _names) =
            build_user_turn("", &[file], root.to_str().unwrap(), "run-1").unwrap();

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
        merge_stream(&mut items, "r1", "reasoning", "ing");
        merge_stream(&mut items, "r1", "assistant", "hello");
        merge_stream(&mut items, "r1", "assistant", " world");
        merge_stream(&mut items, "r1", "reasoning", "again");

        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["text"], "thinking");
        assert_eq!(items[0]["streaming"], false);
        assert_eq!(items[1]["text"], "hello world");
        assert_eq!(items[1]["streaming"], false);
        assert_eq!(items[2]["streaming"], true);
    }

    #[test]
    fn settles_unfinished_tools_at_the_end_of_a_run() {
        let mut items = vec![
            json!({ "kind": "tool", "id": "a", "status": "awaiting", "requestId": "1" }),
            json!({ "kind": "tool", "id": "b", "status": "ok", "output": "done" }),
            json!({ "kind": "assistant", "runId": "r", "text": "hi", "streaming": true }),
        ];
        settle_streaming(&mut items);
        settle_unfinished_tools(&mut items);

        assert_eq!(items[0]["status"], "error");
        assert!(items[0].get("requestId").is_none());
        assert_eq!(items[1]["status"], "ok");
        assert_eq!(items[2]["streaming"], false);
        assert!(!is_blocked(&items));
    }
}
