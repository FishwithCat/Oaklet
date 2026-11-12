//! Tauri host approval adapter for tool calls.
//!
//! The adapter registers a pending request in shared app state, emits the
//! resolution events through `RunContext`, and waits for the frontend or a
//! cancellation to resolve it. Workspace grants and command handlers stay in
//! `lib.rs`.

use std::path::Path;
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::oneshot;

use crate::chat::RunContext;
use crate::tools::{ApprovalDecision, ApprovalRequest, Approver, Cancel};
use crate::{PendingApproval, auto_approves_command};

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
        .ok()
        .and_then(|granted| granted.clone());
    if auto_approves_command(&request.tool, workspace, granted.as_deref()) {
        return true;
    }

    let request_id = state.next_id();
    let (sender, receiver) = oneshot::channel();
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.insert(
            request_id.clone(),
            PendingApproval {
                run_id: ctx.run_id().to_string(),
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
