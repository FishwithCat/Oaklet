use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use super::SessionSummary;
use crate::store::{SessionMeta, Store};
use crate::tools::CancelHandle;
use crate::windows::workspace_for_window;
use crate::worktrees::{self, Checkout, WaitingTurn};
use crate::{AppState, SharedState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorktreeInfo {
    directory: String,
    head: Option<worktrees::Head>,
    checkout: Option<Checkout>,
    missing: bool,
    waiting_text: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    session_id: &'a str,
    phase: &'a str,
}

pub(crate) fn session_in_project(
    state: &AppState,
    window: &str,
    id: &str,
) -> Result<SessionMeta, String> {
    let project = workspace_for_window(state, window)?;
    let meta = state
        .chat
        .store()
        .session_meta(id)?
        .ok_or("This chat no longer exists.")?;
    if Path::new(&meta.workspace) != project {
        return Err("This chat belongs to another workspace.".into());
    }
    Ok(meta)
}

pub(crate) fn view_directory(
    state: &AppState,
    window: &str,
    expected: Option<&str>,
) -> Result<PathBuf, String> {
    let project = workspace_for_window(state, window)?;
    let Some(expected) = expected.map(Path::new) else {
        return Ok(project);
    };
    if expected == project {
        return Ok(project);
    }
    if expected.join(".git").is_file()
        && expected.canonicalize().ok().as_deref() == Some(expected)
        && state.chat.store().worktree_access(&project, expected)?
    {
        return Ok(expected.to_path_buf());
    }
    Err("This checkout is unavailable in the current workspace.".into())
}

pub(super) async fn prepare_send(
    app: &AppHandle,
    state: &AppState,
    window: &str,
    meta: &SessionMeta,
    waiting: &WaitingTurn,
    cancel: &crate::tools::Cancel,
) -> Result<String, String> {
    let store = state.chat.store();
    let mut checkout = store.worktree(&meta.id)?;
    if checkout.archived {
        return Err("This chat is archived. Unarchive it to continue.".into());
    }
    if let Some(request) = waiting
        .request
        .clone()
        .filter(|_| checkout.created.is_empty() && checkout.active.is_empty())
    {
        if meta.turns != 0 {
            return Err("A worktree can only be requested before the first message.".into());
        }
        let _mutation = tokio::select! {
            _ = cancel.cancelled() => return Err("Worktree operation stopped.".into()),
            lock = worktrees::MUTATION.lock() => lock,
        };
        checkout = store.worktree(&meta.id)?;
        if checkout.active.is_empty() {
            let latest = store
                .session_meta(&meta.id)?
                .ok_or("This chat no longer exists.")?;
            if latest.turns != 0 || checkout.archived {
                return Err("A worktree can only be requested before the first message.".into());
            }
            checkout.waiting = Some(waiting.clone());
            store.save_worktree(&meta.id, &checkout, None)?;
            let generated = generated_name(state, &meta.id);
            let entry = worktrees::create(
                Path::new(&meta.workspace),
                request,
                &generated,
                cancel,
                |phase| {
                    let _ = app.emit_to(
                        window,
                        "worktree://progress",
                        Progress {
                            session_id: &meta.id,
                            phase,
                        },
                    );
                },
            )
            .await?;
            save_entry(store, &meta.id, &mut checkout, entry, true)?;
        }
    }
    if let Some(entry) = checkout.active.last() {
        let available = if checkout.restoring {
            Err("This worktree has an incomplete restoration.".into())
        } else {
            worktrees::validate(entry).await
        };
        if let Err(error) = available {
            let _operation = worktrees::MUTATION.lock().await;
            checkout = store.worktree(&meta.id)?;
            if !checkout.archived {
                checkout.waiting = Some(waiting.clone());
                store.save_worktree(&meta.id, &checkout, None)?;
            }
            return Err(error);
        }
    }
    Ok(checkout.directory(&meta.workspace).to_owned())
}

pub(crate) fn generated_name(state: &AppState, id: &str) -> String {
    let id_part: String = id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let suffix = RandomState::new().hash_one(state.next_id()) as u16;
    format!("oaklet-{id_part}-{suffix:04x}")
}

pub(crate) fn save_entry(
    store: &Store,
    session_id: &str,
    checkout: &mut worktrees::SessionWorktree,
    entry: Checkout,
    created: bool,
) -> Result<(), String> {
    if created {
        checkout.created.push(entry.clone());
    }
    checkout.active.push(entry.clone());
    store.save_worktree(session_id, checkout, Some(&entry.path))
}

pub(crate) fn exit(
    store: &Store,
    session_id: &str,
    checkout: &mut worktrees::SessionWorktree,
) -> Result<Checkout, String> {
    let entry = checkout
        .active
        .pop()
        .ok_or("This chat has not entered a worktree.")?;
    let source = Path::new(&entry.source);
    if !source.is_dir() || source.canonicalize().ok().as_deref() != Some(source) {
        return Err("The worktree's source checkout is unavailable.".into());
    }
    store.save_worktree(session_id, checkout, Some(&entry.source))?;
    Ok(entry)
}

#[tauri::command]
pub(crate) async fn session_worktree(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
) -> Result<WorktreeInfo, String> {
    let meta = session_in_project(&state, window.label(), &session_id)?;
    let checkout = state.chat.store().worktree(&session_id)?;
    let directory = checkout.directory(&meta.workspace).to_owned();
    let entry = checkout.active.last();
    let missing = if let Some(entry) = entry {
        checkout.restoring || worktrees::validate(entry).await.is_err()
    } else {
        false
    };
    let path = Path::new(&directory);
    let head = if missing {
        None
    } else {
        worktrees::head(path).await.ok()
    };
    let waiting_text = checkout.waiting.as_ref().map(|waiting| {
        waiting
            .turn
            .display
            .as_ref()
            .and_then(|display| display["text"].as_str())
            .unwrap_or("Message with attachments")
            .to_owned()
    });
    Ok(WorktreeInfo {
        directory,
        head,
        checkout: entry.cloned(),
        missing,
        waiting_text,
    })
}

#[tauri::command]
pub(crate) async fn session_worktree_recover(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    restore: bool,
) -> Result<(), String> {
    session_in_project(&state, window.label(), &session_id)?;
    let (handle, cancel) = CancelHandle::new();
    let _operation = state
        .chat
        .reserve_plan_confirmation(&session_id, window.label(), handle)?;
    let _mutation = worktrees::MUTATION.lock().await;
    let store = state.chat.store();
    let mut checkout = store.worktree(&session_id)?;
    if checkout.archived {
        return Err("Unarchive this chat before restoring its worktree.".into());
    }
    if restore {
        let entry = checkout
            .active
            .last()
            .cloned()
            .ok_or("This chat has no worktree.")?;
        if !checkout
            .created
            .iter()
            .any(|created| created.path == entry.path)
        {
            return Err("Restore this worktree from the chat that created it.".into());
        }
        checkout.restoring = true;
        store.save_worktree(&session_id, &checkout, None)?;
        worktrees::restore(&entry, &cancel, |phase| {
            let _ = app.emit_to(
                window.label(),
                "worktree://progress",
                Progress {
                    session_id: &session_id,
                    phase,
                },
            );
        })
        .await?;
        checkout.restoring = false;
        store.save_worktree(&session_id, &checkout, None)?;
    } else {
        checkout.restoring = false;
        exit(store, &session_id, &mut checkout)?;
    }
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveNotice {
    message: String,
}

#[tauri::command]
pub(crate) async fn session_archive(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    archived: bool,
) -> Result<Option<String>, String> {
    session_in_project(&state, window.label(), &session_id)?;
    let (handle, _) = CancelHandle::new();
    let operation = state
        .chat
        .reserve_plan_confirmation(&session_id, window.label(), handle)?;
    let store = state.chat.store().clone();
    let mut checkout = store.worktree(&session_id)?;
    if checkout.archived == archived {
        return Ok(None);
    }
    checkout.archived = archived;
    store.save_worktree(&session_id, &checkout, None)?;
    if !archived {
        // A pending cleanup can see the new archive state. An admitted one
        // finishes under this lock before the session is returned to History.
        let _cleanup = worktrees::MUTATION.lock().await;
        return Ok(None);
    }
    let keep = state
        .config
        .lock()
        .map_err(|_| "Settings are unavailable.")?
        .keep_worktrees_on_archive;
    let Some(entry) = checkout.created.first().filter(|_| !keep).cloned() else {
        return Ok(None);
    };
    drop(operation);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn(async move {
        let _mutation = worktrees::MUTATION.lock().await;
        if !store
            .worktree(&session_id)
            .is_ok_and(|state| state.archived)
        {
            return;
        }
        let message = match worktrees::cleanup(&store, &entry).await {
            Ok(true) => "Worktree removed. Its branch and commits were kept.".into(),
            Ok(false) => "Chat archived. The worktree was kept.".into(),
            Err(error) => format!("Chat archived. Worktree cleanup failed: {error}"),
        };
        let _ = app.emit_to(&label, "worktree://archive", ArchiveNotice { message });
    });
    Ok(Some(
        "Chat archived. Checking whether its worktree can be removed…".into(),
    ))
}

pub(super) fn archived_history(
    state: &AppState,
    workspace: &Path,
) -> Result<Vec<SessionSummary>, String> {
    Ok(state
        .chat
        .summaries(state.chat.store().archived_sessions(workspace)?))
}
