use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::{Access, Workspace, jobs};

/// Per-run services available to every tool.
pub struct ToolContext {
    pub workspace: Workspace,
    pub cancel: Cancel,
    /// Capability gate shared with the run and the tool registry.
    pub access: Arc<Access>,
    /// Model selected for architecture planning, captured when the run starts.
    pub planning: Option<crate::architecture::PlanningScope>,
    /// Latest architecture draft validation produced by this run.
    pub plan_validation: Option<Arc<Mutex<Option<crate::store::PlanValidation>>>>,
    /// Structured review content submitted with `present_plan`.
    pub plan_details: Option<Arc<Mutex<Option<crate::store::PlanDetails>>>>,
    /// Background jobs owned by the app process, shared across runs.
    pub jobs: std::sync::Arc<jobs::JobRegistry>,
    /// Session that owns spill files.
    pub session_id: String,
    /// Window label, so a background job can report back to its session.
    pub window: String,
    /// App-owned spill directory, outside the workspace.
    pub spill_root: PathBuf,
}

impl ToolContext {
    /// Remember the latest draft validation so run finish can attach it to the
    /// plan event even after compaction drops the tool message from history.
    pub(super) fn record_plan_validation(&self, validation: Option<&crate::store::PlanValidation>) {
        if let Some(slot) = &self.plan_validation {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = validation.cloned();
        }
        // A new draft supersedes review content the agent wrote for an older one.
        if let Some(details) = &self.plan_details {
            *details
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }

    /// Remember the plan review content so run finish can attach it to the plan.
    pub fn record_plan_details(&self, details: &crate::store::PlanDetails) {
        let Some(slot) = &self.plan_details else {
            return;
        };
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(details.clone());
    }
}

/// Cooperative cancellation shared by the agent loop and running tools.
#[derive(Clone)]
pub struct Cancel {
    receiver: watch::Receiver<bool>,
}

/// Owner-side handle that flips a [`Cancel`].
#[derive(Clone)]
pub struct CancelHandle {
    sender: watch::Sender<bool>,
}

impl CancelHandle {
    pub fn new() -> (Self, Cancel) {
        let (sender, receiver) = watch::channel(false);
        (Self { sender }, Cancel { receiver })
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }
}

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Resolve when cancellation is requested, including a request that landed
    /// before this call.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut receiver = self.receiver.clone();
        if self.is_cancelled() {
            return;
        }
        let _ = receiver.changed().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::CancelHandle;

    #[tokio::test]
    async fn cancellation_wakes_a_waiter() {
        let (handle, cancel) = CancelHandle::new();
        let waiter = cancel.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });
        handle.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(cancel.is_cancelled());
    }
}
