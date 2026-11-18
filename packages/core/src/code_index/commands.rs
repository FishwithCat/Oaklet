//! Window-bound desktop adapter; Agent tools call the Rust service directly.
use std::{collections::BTreeMap, time::Duration};

use tauri::{AppHandle, Emitter, State, WebviewWindow};

use super::{CancelHandle, WorkspaceStatus};
use crate::{SharedState, workspace_for_window};

fn bound_workspace(state: &SharedState, label: &str) -> Result<std::path::PathBuf, String> {
    // A destroyed main window must not use the startup fallback to bind itself again.
    if state.windows.lock().unwrap().workspace(label).is_none() {
        return Err("This window has no workspace.".into());
    }
    workspace_for_window(state, label)
}

#[tauri::command]
pub fn code_index_status(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<WorkspaceStatus, String> {
    let root = bound_workspace(&state, window.label())?;
    state.code_index.status(&root).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn code_index_retry(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    project_root: String,
    language: super::Language,
) -> Result<(), String> {
    let root = bound_workspace(&state, window.label())?;
    let (_owner, cancel) = CancelHandle::new();
    state
        .code_index
        .retry(
            &root,
            std::path::Path::new(&project_root),
            language,
            &cancel,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn code_index_rebuild(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let root = bound_workspace(&state, window.label())?;
    state
        .code_index
        .rebuild(&root)
        .await
        .map_err(|e| e.to_string())
}

/// Coalesce server progress and disk verification without coupling service mutations to Tauri.
pub(crate) fn publish(app: AppHandle, state: &SharedState) {
    let weak = std::sync::Arc::downgrade(state);
    tauri::async_runtime::spawn(async move {
        let mut revisions = BTreeMap::new();
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            if *state.exit.lock().unwrap() == crate::shutdown::ExitState::Ready {
                break;
            }
            let windows = state.windows.lock().unwrap();
            emit_changes(
                &windows,
                &state.code_index,
                &mut revisions,
                |label, status| app.emit_to(label, "code-index://changed", status).is_ok(),
            );
        }
    });
}

fn emit_changes(
    windows: &crate::WindowRegistry,
    manager: &super::CodeIndexManager,
    revisions: &mut BTreeMap<String, u64>,
    mut emit: impl FnMut(&str, &WorkspaceStatus) -> bool,
) {
    revisions.retain(|label, _| windows.by_label.contains_key(label));
    for (label, root) in &windows.by_label {
        let Ok(status) = manager.status(root) else {
            continue;
        };
        if revisions.get(label) == Some(&status.revision) {
            continue;
        }
        if emit(label, &status) {
            revisions.insert(label.clone(), status.revision);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn events_follow_window_ownership_and_forget_destroyed_windows() {
        let a = crate::tools::test_support::temp_root("index-events-a");
        let b = crate::tools::test_support::temp_root("index-events-b");
        let manager = Arc::new(super::super::CodeIndexManager::new());
        manager.open_workspace(&a, Default::default()).unwrap();
        manager.open_workspace(&b, Default::default()).unwrap();
        let mut windows = crate::WindowRegistry::default();
        windows.register("main".into(), a.clone()).unwrap();
        windows.register("other".into(), b.clone()).unwrap();
        let mut revisions = BTreeMap::new();
        let mut seen = BTreeMap::new();
        emit_changes(&windows, &manager, &mut revisions, |label, status| {
            seen.insert(label.to_string(), status.workspace.clone());
            true
        });
        assert_eq!(
            seen,
            BTreeMap::from([("main".into(), a.clone()), ("other".into(), b.clone())])
        );
        windows.remove_label("main");
        manager.close_workspace(&a);
        emit_changes(&windows, &manager, &mut revisions, |label, status| {
            assert_eq!(label, "other");
            assert_eq!(status.workspace, b);
            true
        });
        assert!(!revisions.contains_key("main"));
        assert_eq!(
            manager.status(&a).unwrap_err(),
            super::super::IndexError::Closed
        );
        manager.shutdown().await;
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
