//! Window-bound chat commands used by the desktop frontend.
use std::path::Path;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager, State, WebviewWindow};

use super::input::{build_user_turn, session_title};
use super::manual_compaction::{CompactionSlot, compact_session};
use super::mentions;
use super::run::{RunTrigger, start_run};
use super::transcript::view_from_session;
use super::{
    CompactResult, IncomingAttachment, SendResult, SessionOperation, SessionSummary, SessionView,
    resolve_approvals,
};
use crate::SharedState;
use crate::architecture::{self, PlanningScope};
use crate::store::{self, NewUserTurn};
use crate::tools::{CancelHandle, Workspace};
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
    // The user opts into architecture planning by typing the mention; the
    // backend never infers it from the request.
    let trigger = if mentions::has(&text, "arch") {
        RunTrigger::Arch
    } else {
        RunTrigger::User
    };
    let (message, display, attachment_names) =
        build_user_turn(&text, &attachments, &state.spill_root, &session_id)?;
    let title = if meta.turns == 0 {
        let title = session_title(&mentions::strip(&text), &attachment_names);
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
        trigger,
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
pub async fn session_confirm_plan(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    version: u64,
) -> Result<SendResult, String> {
    let window_workspace = workspace_for_window(&state, window.label())?;
    let session_store = state.chat.store.clone();
    let meta = session_store
        .session_meta(&session_id)?
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&meta.workspace) != window_workspace.as_path() {
        return Err(
            "This chat belongs to another workspace. Open that workspace in its own window.".into(),
        );
    }
    if !Path::new(&meta.workspace).is_dir() {
        return Err(format!("Workspace is unavailable: {}", meta.workspace));
    }

    let (cancel_handle, cancel) = CancelHandle::new();
    let _confirmation =
        state
            .chat
            .reserve_plan_confirmation(&session_id, window.label(), cancel_handle.clone())?;
    let plan = session_store
        .latest_plan(&session_id)?
        .ok_or_else(|| "This plan is no longer awaiting confirmation.".to_string())?;
    if plan.version != version || plan.status != store::PlanStatus::Pending {
        return Err("This plan version is no longer awaiting confirmation.".into());
    }

    if let Some(validation) = plan.validation_for_confirmation()? {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Stopped.".into()),
            result = verify_confirmation(&app, &meta.workspace, validation, cancel.clone()) => result?,
        }
    }

    let text = confirmation_message(&plan);
    let turn = NewUserTurn {
        message: json!({ "role": "user", "content": text }),
        display: Some(json!({ "text": format!("Implement plan v{}", plan.version) })),
        title: None,
    };
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    config.workspace = meta.workspace.clone();

    let (run_id, view) = start_run(
        &app,
        state.inner(),
        window.label(),
        &session_id,
        RunTrigger::Plan(version),
        config,
        turn,
        cancel,
        cancel_handle,
    )?;
    state.chat.live().wakes.remove(&session_id);
    let session = view
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(SendResult { run_id, session })
}

async fn verify_confirmation(
    app: &AppHandle,
    workspace: &str,
    validation: &store::PlanValidation,
    cancel: crate::tools::Cancel,
) -> Result<(), String> {
    let model = verify_baseline(workspace, validation).await?;
    let node = architecture::find_node()
        .ok_or("LikeC4 requires Node.js to recheck this architecture draft.")?;
    let helper = architecture::find_helper(app)
        .ok_or("The LikeC4 compiler helper is missing. Reinstall Oaklet.")?;
    let draft = crate::tools::draft_path(&model.root, &model.sources);
    let request = json!({
        "version": 1,
        "sources": model.sources,
        "diff": { "path": draft, "text": validation.dsl },
    });
    let result = architecture::run_helper_watch(&node, &helper, &request, cancel).await?;
    if !result.ok {
        return Err(format!(
            "Revise the architecture draft before implementing this plan. {}",
            result
                .error
                .as_deref()
                .unwrap_or("LikeC4 could not verify the draft.")
        ));
    }
    // The source files can change while the compiler runs.
    verify_baseline(workspace, validation).await?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBaseline {
    pub version: u64,
    /// False for a text-only plan, which has no model baseline to compare.
    pub checked: bool,
    pub baseline_changed: bool,
}

/// Report whether a verified plan's architecture baseline is still current.
#[tauri::command]
pub async fn session_plan_baseline(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    version: u64,
) -> Result<PlanBaseline, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    let store = state.chat.store.clone();
    let load_id = session_id.clone();
    let meta = tauri::async_runtime::spawn_blocking(move || store.session_meta(&load_id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&meta.workspace) != workspace.as_path() {
        return Err("This chat belongs to another workspace.".into());
    }

    let plan = state
        .chat
        .store
        .latest_plan(&session_id)?
        .ok_or_else(|| "This plan is no longer available.".to_string())?;
    if plan.version != version {
        return Err("This plan version is no longer current.".into());
    }
    let Some(validation) = plan.validation.filter(|validation| {
        validation.status == store::PlanValidationStatus::Verified && validation.valid
    }) else {
        return Ok(PlanBaseline {
            version,
            checked: false,
            baseline_changed: false,
        });
    };
    Ok(PlanBaseline {
        version,
        checked: true,
        baseline_changed: baseline_changed(&meta.workspace, &validation).await?,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanComparison {
    pub version: u64,
    #[serde(flatten)]
    pub compare: architecture::PlanCompare,
}

/// Lay out one affected view for a saved plan version.
#[tauri::command]
pub async fn session_plan_compare(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    session_id: String,
    version: u64,
    view_id: String,
) -> Result<PlanComparison, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    let store = state.chat.store.clone();
    let load_id = session_id.clone();
    let meta = tauri::async_runtime::spawn_blocking(move || store.session_meta(&load_id))
        .await
        .map_err(|error| format!("Could not read the session: {error}"))??
        .ok_or_else(|| "This session no longer exists.".to_string())?;
    if Path::new(&meta.workspace) != workspace.as_path() {
        return Err("This chat belongs to another workspace.".into());
    }
    let plan = state
        .chat
        .store
        .latest_plan(&session_id)?
        .ok_or_else(|| "This plan is no longer available.".to_string())?;
    if plan.version != version {
        return Err("This plan version is no longer current.".into());
    }
    let validation = plan
        .validation
        .filter(|validation| {
            validation.status == store::PlanValidationStatus::Verified && validation.valid
        })
        .ok_or_else(|| "This plan has no verified architecture preview.".to_string())?;
    let compare =
        architecture::plan_compare(&app, Path::new(&meta.workspace), &validation, &view_id).await?;
    Ok(PlanComparison { version, compare })
}

/// Re-read the selected model and check that the plan's baseline still holds.
async fn verify_baseline(
    workspace: &str,
    validation: &store::PlanValidation,
) -> Result<architecture::PlanningModel, String> {
    let model = load_baseline(workspace, validation).await?;
    if model.digest != validation.source_digest {
        Err(
            "The architecture model changed after this plan was validated. Ask for an updated plan, then confirm the new version."
                .into(),
        )
    } else {
        Ok(model)
    }
}

async fn baseline_changed(
    workspace: &str,
    validation: &store::PlanValidation,
) -> Result<bool, String> {
    Ok(load_baseline(workspace, validation).await?.digest != validation.source_digest)
}

async fn load_baseline(
    workspace: &str,
    validation: &store::PlanValidation,
) -> Result<architecture::PlanningModel, String> {
    let path = workspace.to_string();
    let scope = PlanningScope {
        model_root: validation.model_root.clone(),
        include_ignored: validation.include_ignored,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let workspace = Workspace::new(&path)?;
        architecture::planning_model(&workspace, Some(&scope))
    })
    .await
    .map_err(|error| format!("Could not check the architecture model: {error}"))?
}

fn confirmation_message(plan: &store::PlanRecord) -> String {
    let plan_text = match &plan.details {
        Some(details) => details_message(details),
        None => plan.steps.trim().to_string(),
    };
    let mut text = format!(
        "The user confirmed plan version {} from the plan card. Implement it now and follow the plan below.\n\n{}",
        plan.version, plan_text
    );
    if let Some(validation) = &plan.validation
        && validation.status == store::PlanValidationStatus::Verified
        && validation.valid
    {
        text.push_str(&format!(
            "\n\nArchitecture model `{}` (baseline `{}`). Apply this validated LikeC4 diff and keep the architecture sources consistent with the code you change:\n\n```likec4\n{}\n```",
            validation.model_root,
            validation.source_digest,
            validation.dsl.trim()
        ));
    } else {
        text.push_str("\n\nThe user confirmed only these implementation steps. Treat any unverified architecture draft in the conversation as reference and do not apply it.");
    }
    text
}

fn details_message(details: &store::PlanDetails) -> String {
    let mut text = format!(
        "Objective:\n{}\n\nChanges:\n{}",
        details.objective.trim(),
        details.changes.trim()
    );
    text.push_str("\n\nImplementation steps:");
    for (index, step) in details.steps.iter().enumerate() {
        text.push_str(&format!("\n{}. {}", index + 1, step.title.trim()));
        text.push_str(&format!("\n   Outcome: {}", step.outcome.trim()));
        if !step.files.is_empty() {
            text.push_str(&format!("\n   Files: {}", step.files.join(", ")));
        }
        if !step.components.is_empty() {
            text.push_str(&format!(
                "\n   Architecture: {}",
                step.components.join(", ")
            ));
        }
        if !step.depends_on.is_empty() {
            text.push_str(&format!(
                "\n   Depends on: {}",
                step.depends_on
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        text.push_str(&format!("\n   Verify: {}", step.verification.trim()));
    }
    if !details.risks.is_empty() {
        text.push_str("\n\nRisks:\n");
        for risk in &details.risks {
            text.push_str(&format!("- {}\n", risk.trim()));
        }
    }
    if !details.questions.is_empty() {
        text.push_str("\nOpen questions:\n");
        for question in &details.questions {
            text.push_str(&format!("- {}\n", question.trim()));
        }
    }
    text
}

#[tauri::command]
pub fn session_stop(window: WebviewWindow, state: State<'_, SharedState>, session_id: String) {
    let run_id = {
        let live = state.chat.live();
        live.stop_session_operations(&session_id, window.label());
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
            || live.confirming.contains_key(&session_id)
        {
            return Err("Stop the run before compacting this chat.".into());
        }
        let (handle, cancel) = CancelHandle::new();
        live.compacting.insert(
            session_id.clone(),
            SessionOperation {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{PlanRecord, PlanStatus, PlanValidation, PlanValidationStatus};
    use std::path::Path;

    fn plan(validation: Option<PlanValidation>) -> PlanRecord {
        PlanRecord {
            version: 2,
            status: PlanStatus::Pending,
            steps: "1. Add the worker.".into(),
            details: None,
            validation,
        }
    }

    #[test]
    fn a_verified_plan_carries_its_baseline_and_diff() {
        let text = confirmation_message(&plan(Some(PlanValidation {
            status: PlanValidationStatus::Verified,
            valid: true,
            model_root: "architecture".into(),
            include_ignored: false,
            source_digest: "abc123".into(),
            diff_id: Some("worker".into()),
            dsl: "diff worker 'Worker' {}".into(),
            diff: None,
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            overlaps: Vec::new(),
            error: None,
        })));
        assert!(text.contains("plan version 2"));
        assert!(text.contains("1. Add the worker."));
        assert!(text.contains("architecture"));
        assert!(text.contains("abc123"));
        assert!(text.contains("diff worker 'Worker' {}"));
    }

    #[test]
    fn a_text_plan_has_no_unverified_diff() {
        let mut record = plan(Some(PlanValidation {
            status: PlanValidationStatus::Unavailable,
            valid: false,
            model_root: String::new(),
            include_ignored: false,
            source_digest: String::new(),
            diff_id: None,
            dsl: "diff unreviewed {}".into(),
            diff: None,
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            overlaps: Vec::new(),
            error: Some("No LikeC4 model was found.".into()),
        }));
        assert!(record.validation_for_confirmation().unwrap().is_none());
        let text = confirmation_message(&record);
        assert!(text.contains("plan version 2"));
        assert!(text.contains("1. Add the worker."));
        assert!(!text.contains("diff unreviewed"));

        record.validation.as_mut().unwrap().status = PlanValidationStatus::Invalid;
        assert!(record.validation_for_confirmation().is_err());
    }

    #[test]
    fn a_structured_plan_carries_steps_risks_and_questions() {
        let mut record = plan(None);
        record.details = Some(store::PlanDetails {
            objective: "Add a worker".into(),
            changes: "Route jobs through a queue.".into(),
            steps: vec![store::PlanStep {
                title: "Add the queue".into(),
                outcome: "Jobs can be enqueued.".into(),
                files: vec!["src/queue.rs".into()],
                components: vec!["api.queue".into()],
                depends_on: Vec::new(),
                verification: "cargo test queue".into(),
            }],
            risks: vec!["Delivery is at least once.".into()],
            questions: vec!["Should retries be bounded?".into()],
        });
        let text = confirmation_message(&record);
        assert!(text.contains("Objective:\nAdd a worker"));
        assert!(text.contains("1. Add the queue"));
        assert!(text.contains("Verify: cargo test queue"));
        assert!(text.contains("Risks:\n- Delivery is at least once."));
        assert!(text.contains("Open questions:\n- Should retries be bounded?"));
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn confirmation_rechecks_the_model_baseline() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../architecture-helper/test/fixtures/valid")
            .canonicalize()
            .unwrap();
        let root = crate::tools::test_support::temp_root("plan-confirm-baseline");
        copy_tree(&fixture, &root);
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let model = architecture::planning_model(&workspace, None).unwrap();
        let validation = PlanValidation {
            status: PlanValidationStatus::Verified,
            valid: true,
            model_root: model.root,
            include_ignored: false,
            source_digest: model.digest,
            diff_id: Some("worker".into()),
            dsl: "diff worker 'Worker' {}".into(),
            diff: None,
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            overlaps: Vec::new(),
            error: None,
        };
        verify_baseline(root.to_str().unwrap(), &validation)
            .await
            .unwrap();

        let source = root.join("architecture/model.c4");
        let mut text = std::fs::read_to_string(&source).unwrap();
        text.push_str("\n// changed\n");
        std::fs::write(&source, text).unwrap();
        let error = verify_baseline(root.to_str().unwrap(), &validation)
            .await
            .unwrap_err();
        assert!(error.contains("changed"));
        std::fs::remove_dir_all(root).ok();
    }
}
