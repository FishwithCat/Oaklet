use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::store::WorkspaceUsage;
use crate::{SharedState, log, spill};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    pub database_bytes: u64,
    pub spill_bytes: u64,
    pub spill_files: usize,
    pub sessions: usize,
    pub messages: usize,
    pub workspaces: Vec<WorkspaceUsage>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeResult {
    pub deleted: usize,
    pub usage: StorageUsage,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPurged {
    workspace: Option<String>,
}

fn read_usage(state: &SharedState) -> Result<StorageUsage, String> {
    let stored = state.chat.store().usage()?;
    let (spill_bytes, spill_files) = spill::usage(&state.spill_root);
    let workspaces = stored
        .workspaces
        .into_iter()
        .map(|row| WorkspaceUsage {
            label: crate::paths::display_path(std::path::Path::new(&row.workspace)),
            ..row
        })
        .collect();
    Ok(StorageUsage {
        database_bytes: stored.database_bytes,
        spill_bytes,
        spill_files,
        sessions: stored.sessions,
        messages: stored.messages,
        workspaces,
    })
}

#[tauri::command]
pub async fn storage_usage(state: State<'_, SharedState>) -> Result<StorageUsage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || read_usage(&state))
        .await
        .map_err(|error| format!("Could not read storage usage: {error}"))?
}

#[tauri::command]
pub async fn storage_purge(
    app: AppHandle,
    state: State<'_, SharedState>,
    workspace: Option<String>,
) -> Result<PurgeResult, String> {
    if state.chat.any_busy() {
        return Err("Stop the running chat before deleting saved history.".into());
    }
    let state = state.inner().clone();
    let scope = workspace.clone();
    let (deleted, usage) = tauri::async_runtime::spawn_blocking(move || {
        let store = state.chat.store();
        let ids = match scope.as_deref() {
            Some(workspace) => {
                if !store.has_workspace(workspace)? {
                    return Err("No saved history belongs to that workspace.".to_string());
                }
                store.purge(Some(workspace))?
            }
            None => store.purge(None)?,
        };
        for id in &ids {
            spill::remove_session(&state.spill_root, id);
        }
        Ok::<_, String>((ids.len(), read_usage(&state)?))
    })
    .await
    .map_err(|error| format!("Could not delete saved history: {error}"))??;
    log::key(format_args!(
        "[oaklet] deleted {deleted} saved session(s) of {}",
        workspace.as_deref().unwrap_or("every workspace")
    ));
    let _ = app.emit("sessions://purged", HistoryPurged { workspace });
    Ok(PurgeResult { deleted, usage })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use crate::spill;
    use crate::store::{NewUserTurn, Store};
    use crate::tools::test_support::temp_root;

    fn disk_bytes(path: &Path) -> u64 {
        ["", "-wal", "-shm"]
            .into_iter()
            .filter_map(|suffix| {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                std::fs::metadata(PathBuf::from(name)).ok()
            })
            .map(|metadata| metadata.len())
            .sum()
    }

    fn turn(text: &str) -> NewUserTurn {
        NewUserTurn {
            message: json!({ "role": "user", "content": text }),
            display: None,
            title: Some("chat".into()),
        }
    }

    #[test]
    fn purging_history_reclaims_the_database_and_its_spill_files() {
        let root = temp_root("storage-purge");
        let path = root.join("oaklet.db");
        let spill_root = root.join("spill");
        let store = Store::open(&path).unwrap();
        let text = "x".repeat(8 * 1024);

        for index in 0..40 {
            let id = format!("s{index}");
            store.create_session(&id, "/work/a", index).unwrap();
            store
                .begin_run(&id, &format!("r{index}"), &turn(&text), index + 1)
                .unwrap();
        }
        store.create_session("keep", "/work/b", 100).unwrap();
        store.begin_run("keep", "rk", &turn("short"), 101).unwrap();
        let spilled = spill::save(&spill_root, "s7", "output", &text).unwrap();

        let before = disk_bytes(&path);
        assert!(
            before > 300 * 1024,
            "the database should hold the history: {before}"
        );

        assert!(store.has_workspace("/work/a").unwrap());
        let ids = store.purge(Some("/work/a")).unwrap();
        assert_eq!(ids.len(), 40);
        for id in &ids {
            spill::remove_session(&spill_root, id);
        }

        assert!(!spilled.path.exists());
        assert!(!store.has_workspace("/work/a").unwrap());
        let after = disk_bytes(&path);
        assert!(
            after * 2 < before,
            "the purge should hand the pages back: {before} -> {after}"
        );
        let usage = store.usage().unwrap();
        assert_eq!(usage.sessions, 1);
        assert_eq!(usage.messages, 1);
        assert_eq!(usage.workspaces.len(), 1);
        assert_eq!(usage.workspaces[0].workspace, "/work/b");
        assert_eq!(spill::usage(&spill_root), (0, 0));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn deleting_every_workspace_leaves_a_store_that_still_accepts_chats() {
        let root = temp_root("storage-purge-all");
        let path = root.join("oaklet.db");
        let store = Store::open(&path).unwrap();
        store.create_session("a", "/work/a", 1).unwrap();
        store.begin_run("a", "r1", &turn("hello"), 2).unwrap();
        store.create_session("b", "/work/b", 3).unwrap();
        store.begin_run("b", "r2", &turn("hello"), 4).unwrap();

        assert_eq!(store.purge(None).unwrap().len(), 2);
        let usage = store.usage().unwrap();
        assert_eq!(usage.sessions, 0);
        assert_eq!(usage.messages, 0);
        assert!(usage.workspaces.is_empty());

        store.create_session("fresh", "/work/a", 5).unwrap();
        store.begin_run("fresh", "r3", &turn("again"), 6).unwrap();
        assert_eq!(store.usage().unwrap().messages, 1);

        std::fs::remove_dir_all(&root).ok();
    }
}
