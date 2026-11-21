//! Window-bound chat commands used by the desktop frontend.
use std::path::Path;

use tauri::{AppHandle, Manager, State, WebviewWindow};

use super::input::{build_user_turn, session_title};
use super::manual_compaction::{CompactionSlot, compact_session};
use super::run::{RunTrigger, start_run};
use super::transcript::view_from_session;
use super::{
    CompactResult, CompactionHandle, IncomingAttachment, SendResult, SessionSummary, SessionView,
    resolve_approvals,
};
use crate::SharedState;
use crate::store::{self, NewUserTurn};
use crate::tools::CancelHandle;
use crate::windows::workspace_for_window;

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
        RunTrigger::User,
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
