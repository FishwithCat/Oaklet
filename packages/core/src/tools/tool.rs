use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{FileDiff, ToolAccess, ToolContext, ToolOutput};

/// One capability exposed to the model.
pub trait Tool: Send + Sync {
    /// Model-facing name, also used by the registry for lookup.
    fn name(&self) -> &str;
    /// Model-facing description.
    fn description(&self) -> &str;
    /// JSON Schema for the call arguments.
    fn parameters(&self) -> Value;
    /// One system-prompt rule; omitted when empty.
    fn guidance(&self) -> &str {
        ""
    }
    /// Short pending-call summary (usually the touched path or command).
    fn summary(&self, _args: &Value) -> String {
        String::new()
    }
    /// Display-only diff shown while the call is pending, before it runs.
    fn preview(&self, _ctx: &ToolContext, _args: &Value) -> Vec<FileDiff> {
        Vec::new()
    }
    /// Approval to request before running this call, if any.
    fn approval(&self, _args: &Value) -> Option<ApprovalRequest> {
        None
    }
    /// What this tool may do to the workspace. Everything defaults to writing.
    fn access(&self) -> ToolAccess {
        ToolAccess::Execute
    }
    /// Plan tools are only offered while an architecture plan is under way.
    fn plan_only(&self) -> bool {
        false
    }
    /// Whether this call may overlap with sibling read-only calls. The agent
    /// only groups runs of exact `true`; everything else is a barrier. Opt in
    /// only when the body neither writes nor asks for approval.
    fn is_concurrency_safe(&self) -> bool {
        false
    }
    /// Cooperative deadline enforced by the pipeline.
    fn timeout(&self) -> Option<Duration> {
        None
    }
    /// Run one validated call and return the model-facing output.
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>>;
}

/// Human-facing details for one approval card.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub tool: String,
    pub prompt: String,
    pub detail: String,
    pub args: Value,
}

/// Outcome of an approval prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    Cancelled,
}

/// Host-side approval prompt used by the execution pipeline.
pub trait Approver: Send + Sync {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision>;
}

/// A tool-owned failure. The pipeline turns every variant into an error result
/// for the model instead of ending the turn.
#[derive(Debug)]
pub enum ToolError {
    Unknown(String),
    InvalidArguments(String),
    Failed(String),
    Timeout(Duration),
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(formatter, "Unknown tool: {name}"),
            Self::InvalidArguments(message) => write!(formatter, "Invalid arguments: {message}"),
            Self::Failed(message) => formatter.write_str(message),
            Self::Timeout(limit) => {
                write!(formatter, "Timed out after {}s.", limit.as_secs())
            }
            Self::Cancelled => formatter.write_str("Cancelled."),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

pub(super) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}
