//! Model-facing tool definitions and their execution pipeline.
//!
//! Each tool owns its schema, system-prompt guidance, approval policy, timeout,
//! and body. The registry projects the model-facing schemas; the pipeline owns
//! lookup, approval, deadlines, and result normalization. The agent loop never
//! matches on a tool name.

mod access;
mod comments;
mod context;
mod diagnostics;
mod diff;
mod fs;
pub(crate) mod history;
pub(crate) mod image;
pub(crate) mod jobs;
mod likec4;
mod mcp;
mod outline;
mod output;
mod patch;
mod plan;
mod plan_diff;
mod plan_present;
pub(crate) mod principles;
mod registry;
pub(crate) mod search;
mod shell;
pub(crate) mod skill;
mod structure;
#[cfg(test)]
pub(crate) mod test_support;
mod tool;
pub(crate) mod web;
mod web_fetch;
mod workspace;
pub(crate) mod worktree;

pub(crate) use access::PLAN_BASELINE_CHANGED;
pub use access::{Access, ArchitectureImpact, Assessment, Intent, ToolAccess};
pub use context::{Cancel, CancelHandle, ToolContext};
pub use diff::FileDiff;
use output::TOOL_OUTPUT_THRESHOLD_CHARS;
pub use output::{ProcessOutcome, ToolImage, ToolOutcome, ToolOutput};
pub(crate) use output::{cap, spill_pointer};
pub(crate) use plan_diff::draft_path;
pub use registry::ToolRegistry;
use tool::parse_args;
pub use tool::{ApprovalDecision, ApprovalRequest, Approver, Tool, ToolError};
pub use workspace::Workspace;

/// Release process-wide tool resources during app shutdown.
pub(crate) fn shutdown() {
    mcp::shutdown();
}

/// Release servers and caches owned by one workspace when its window closes.
pub(crate) fn shutdown_workspace(workspace: &std::path::Path) {
    mcp::evict_workspace(workspace);
}
