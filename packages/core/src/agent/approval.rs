//! Tauri host approval adapter for tool calls.
//!
//! The adapter registers a pending request in shared app state, emits the
//! resolution events through `RunContext`, and waits for the frontend or a
//! cancellation to resolve it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tauri::{State, WebviewWindow};
use tokio::sync::oneshot;

use crate::SharedState;
use crate::chat::RunContext;
use crate::store::Store;
use crate::tools::{ApprovalDecision, ApprovalRequest, Approver, Cancel};
use crate::windows::workspace_for_window;

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

pub(super) struct TauriApprover<'a> {
    pub(super) ctx: &'a RunContext,
    pub(super) call_id: &'a str,
    pub(super) workspace: &'a Path,
    pub(super) cancel: &'a Cancel,
}

impl Approver for TauriApprover<'_> {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            let allowed =
                request_approval(self.ctx, self.call_id, self.workspace, request, self.cancel)
                    .await;
            if allowed {
                ApprovalDecision::Allow
            } else if self.cancel.is_cancelled() {
                ApprovalDecision::Cancelled
            } else {
                ApprovalDecision::Deny
            }
        })
    }
}

async fn request_approval(
    ctx: &RunContext,
    call_id: &str,
    workspace: &Path,
    request: ApprovalRequest,
    cancel: &Cancel,
) -> bool {
    if cancel.is_cancelled() {
        return false;
    }

    let state = ctx.state();
    let granted = state
        .command_allow_all
        .lock()
        .map(|granted| granted.clone())
        .unwrap_or_default();
    if auto_approves_command(&request.tool, workspace, &granted, state.chat.store()) {
        return true;
    }

    let request_id = state.next_id();
    let (sender, receiver) = oneshot::channel();
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.insert(
            request_id.clone(),
            PendingApproval {
                run_id: ctx.run_id().to_string(),
                window: ctx.window_label().to_string(),
                workspace: workspace.to_path_buf(),
                sender,
            },
        );
    } else {
        return false;
    }
    if cancel.is_cancelled() {
        if let Ok(mut approvals) = state.approvals.lock() {
            approvals.remove(&request_id);
        }
        return false;
    }

    ctx.approval(
        call_id,
        &request_id,
        &request.tool,
        request.args,
        &request.prompt,
        &request.detail,
    );

    let decision = tokio::time::timeout(APPROVAL_TIMEOUT, receiver).await;
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.remove(&request_id);
    }

    let allowed = matches!(decision, Ok(Ok(true)));
    ctx.approval_resolved(call_id, allowed);
    allowed
}

pub struct PendingApproval {
    pub run_id: String,
    pub window: String,
    /// Workspace the command will run in, so a grant stays scoped to it.
    pub workspace: PathBuf,
    pub sender: oneshot::Sender<bool>,
}

/// Only shell commands in the workspace the user granted may skip the approval
/// prompt; every other request still asks. A worktree checkout belongs to the
/// project workspace it was created from, so the grant covers it too.
fn auto_approves_command(
    tool: &str,
    workspace: &Path,
    granted: &HashSet<PathBuf>,
    store: &Store,
) -> bool {
    tool == "run_command"
        && (granted.contains(workspace)
            || granted
                .iter()
                .any(|project| store.worktree_access(project, workspace).unwrap_or(false)))
}

#[tauri::command]
pub(crate) fn approve_tool(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    request_id: String,
    allow: bool,
    always: bool,
) -> Result<(), String> {
    let mut approvals = state
        .approvals
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?;
    let Some(pending) = approvals.remove(&request_id) else {
        return Err("This approval request has expired.".into());
    };
    if pending.window != window.label() {
        approvals.insert(request_id, pending);
        return Err("This approval request belongs to another window.".into());
    }
    drop(approvals);

    pending
        .sender
        .send(allow)
        .map_err(|_| "The agent is no longer waiting for this action.".to_string())?;

    // A grant is session-only and scoped to the workspace the approved command
    // runs in, not to whatever workspace settings show later.
    if allow
        && always
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        granted.insert(pending.workspace);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn command_allow_all_active(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<bool, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    Ok(state
        .command_allow_all
        .lock()
        .map(|granted| granted.contains(&workspace))
        .unwrap_or(false))
}

#[tauri::command]
pub(crate) fn revoke_command_allow_all(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let workspace = workspace_for_window(&state, window.label())?;
    if let Ok(mut granted) = state.command_allow_all.lock() {
        granted.remove(&workspace);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn grant_command_allow_all(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let workspace = workspace_for_window(&state, window.label())?;
    if let Ok(mut granted) = state.command_allow_all.lock() {
        granted.insert(workspace);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::worktrees::{Checkout, SessionWorktree};

    #[test]
    fn auto_approves_only_commands_in_the_granted_workspace_or_its_worktrees() {
        let store = Store::in_memory().unwrap();
        let workspace = Path::new("/tmp/oaklet-workspace");
        let other = Path::new("/tmp/oaklet-other");
        let worktree = Path::new("/tmp/oaklet-workspace/.agents/worktrees/fix");
        let granted = HashSet::from([workspace.to_path_buf()]);

        assert!(auto_approves_command(
            "run_command",
            workspace,
            &granted,
            &store
        ));
        assert!(!auto_approves_command(
            "run_command",
            other,
            &granted,
            &store
        ));
        assert!(!auto_approves_command(
            "run_command",
            workspace,
            &HashSet::new(),
            &store
        ));
        assert!(!auto_approves_command(
            "write_file",
            workspace,
            &granted,
            &store
        ));

        store
            .create_session("s", &workspace.to_string_lossy(), 1)
            .unwrap();
        let entry = Checkout {
            path: worktree.to_string_lossy().into_owned(),
            source: workspace.to_string_lossy().into_owned(),
            common_dir: "/tmp/oaklet-workspace/.git".into(),
            branch: Some("fix".into()),
            base_commit: Some("baseline".into()),
        };
        store
            .save_worktree(
                "s",
                &SessionWorktree {
                    active: vec![entry.clone()],
                    created: vec![entry],
                    ..Default::default()
                },
                None,
            )
            .unwrap();

        assert!(auto_approves_command(
            "run_command",
            worktree,
            &granted,
            &store
        ));
        assert!(!auto_approves_command(
            "run_command",
            worktree,
            &HashSet::new(),
            &store
        ));
        assert!(!auto_approves_command(
            "run_command",
            other,
            &granted,
            &store
        ));
    }
}
