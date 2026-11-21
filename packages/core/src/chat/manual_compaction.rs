//! Manual context reduction; persist only a validated replacement.
use std::path::Path;
use std::sync::{Arc, Mutex};

use tauri::AppHandle;

use super::{ChatState, run::trim_incomplete_tool_block};
use crate::store::{self, ContextUsage};
use crate::{SharedState, compaction, context};

/// Clears the manual-compaction flag even if the command is dropped mid-way.
pub(super) struct CompactionSlot<'a> {
    pub(super) chat: &'a ChatState,
    pub(super) session_id: String,
}

impl Drop for CompactionSlot<'_> {
    fn drop(&mut self) {
        self.chat.live().compacting.remove(&self.session_id);
    }
}

pub(super) async fn compact_session(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::SessionOperation;
    use crate::tools::CancelHandle;

    #[test]
    fn dropping_a_compaction_slot_releases_the_session() {
        let state = ChatState::in_memory();
        let (handle, _cancel) = CancelHandle::new();
        state.live().compacting.insert(
            "s1".into(),
            SessionOperation {
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
}
