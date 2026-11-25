use serde::Serialize;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use super::{Event, TerminalSession, spawn};
use crate::SharedState;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    id: String,
    sequence: u64,
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalExit {
    id: String,
    code: Option<u32>,
}

#[tauri::command]
pub(crate) fn terminal_open(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    id: String,
    cols: u16,
    rows: u16,
    workspace: Option<String>,
) -> Result<(), String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    let window_label = window.label().to_string();

    let mut sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    let (session, events) = spawn(&workspace, cols, rows)?;
    let output_flow = session.output_flow.clone();
    sessions.insert(
        id.clone(),
        TerminalSession {
            session,
            window: window_label.clone(),
        },
    );
    drop(sessions);

    let shared = state.inner().clone();
    let session_id = id;
    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(event) = events.recv() {
            match event {
                Event::Output(data) => {
                    let Some(sequence) = output_flow.reserve(data.len()) else {
                        break;
                    };
                    if app
                        .emit_to(
                            &window_label,
                            "terminal://output",
                            TerminalOutput {
                                id: session_id.clone(),
                                sequence,
                                data,
                            },
                        )
                        .is_err()
                    {
                        break;
                    }
                }
                Event::Exit { code } => {
                    let _ = app.emit_to(
                        &window_label,
                        "terminal://exit",
                        TerminalExit {
                            id: session_id.clone(),
                            code,
                        },
                    );
                    break;
                }
            }
        }
        if let Ok(mut sessions) = shared.terminal.lock() {
            sessions.remove(&session_id);
        }
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn terminal_ack(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    id: String,
    sequence: u64,
) {
    if let Ok(sessions) = state.terminal.lock()
        && let Some(session) = sessions.get(&id)
        && session.window == window.label()
    {
        session.session.output_flow.acknowledge(sequence);
    }
}

#[tauri::command]
pub(crate) fn terminal_write(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let mut sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    let Some(session) = sessions.get_mut(&id) else {
        return Ok(());
    };
    if session.window != window.label() {
        return Ok(());
    }
    session.session.write(data.as_bytes())
}

#[tauri::command]
pub(crate) fn terminal_resize(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    if let Some(session) = sessions.get(&id)
        && session.window == window.label()
    {
        session.session.resize(cols, rows)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn terminal_close(window: WebviewWindow, state: State<'_, SharedState>, id: String) {
    if let Ok(mut sessions) = state.terminal.lock()
        && sessions
            .get(&id)
            .is_some_and(|session| session.window == window.label())
    {
        sessions.remove(&id);
    }
}
