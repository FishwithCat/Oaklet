//! Model-facing tool definitions and their execution pipeline.
//!
//! Each tool owns its schema, system-prompt guidance, approval policy, timeout,
//! and body. The registry projects the model-facing schemas; the pipeline owns
//! lookup, approval, deadlines, and result normalization. The agent loop never
//! matches on a tool name.

use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;

mod fs;
mod shell;
mod workspace;

pub use workspace::Workspace;

const MAX_TOOL_OUTPUT_CHARS: usize = 30_000;

/// One capability exposed to the model.
pub trait Tool: Send + Sync {
    /// Model-facing name, also used by the registry for lookup.
    fn name(&self) -> &'static str;
    /// Model-facing description.
    fn description(&self) -> &'static str;
    /// JSON Schema for the call arguments.
    fn parameters(&self) -> Value;
    /// One system-prompt rule; omitted when empty.
    fn guidance(&self) -> &'static str {
        ""
    }
    /// Short pending-call summary (usually the touched path or command).
    fn summary(&self, _args: &Value) -> String {
        String::new()
    }
    /// Approval to request before running this call, if any.
    fn approval(&self, _args: &Value) -> Option<ApprovalRequest> {
        None
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
    ) -> BoxFuture<'a, Result<String, ToolError>>;
}

/// Human-facing details for one approval card.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub tool: &'static str,
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

/// Normalized result handed back to the agent loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
}

impl ToolOutcome {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            ok: true,
            output: cap(output.into(), MAX_TOOL_OUTPUT_CHARS),
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: cap(message.into(), MAX_TOOL_OUTPUT_CHARS),
        }
    }
}

/// Per-run services available to every tool.
pub struct ToolContext {
    pub workspace: Workspace,
    pub cancel: Cancel,
}

/// Cooperative cancellation shared by the agent loop and running tools.
#[derive(Clone)]
pub struct Cancel {
    receiver: watch::Receiver<bool>,
}

/// Owner-side handle that flips a [`Cancel`].
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

/// All tools available to the agent, in model-visible order.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: vec![
                Box::new(fs::ReadFile),
                Box::new(fs::WriteFile),
                Box::new(fs::EditFile),
                Box::new(fs::ListDir),
                Box::new(shell::RunCommand),
            ],
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(Box::as_ref)
    }

    /// Model-facing `tools` payload for one chat request.
    pub fn schemas(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name(),
                            "description": tool.description(),
                            "parameters": tool.parameters(),
                        }
                    })
                })
                .collect(),
        )
    }

    /// One prompt rule per tool that declares guidance.
    pub fn guidance(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.tools
            .iter()
            .map(|tool| tool.guidance())
            .filter(|text| !text.is_empty())
    }

    /// Pending-call summary for a tool, empty when the call is not visible.
    pub fn summary(&self, name: &str, args: &Value) -> String {
        self.get(name)
            .map(|tool| tool.summary(args))
            .unwrap_or_default()
    }

    /// Look up, ask, run under a deadline, and normalize one call.
    pub async fn execute(
        &self,
        ctx: &ToolContext,
        approver: &dyn Approver,
        name: &str,
        args: Value,
    ) -> ToolOutcome {
        let Some(tool) = self.get(name) else {
            return ToolOutcome::failure(ToolError::Unknown(name.to_string()).to_string());
        };

        if let Some(request) = tool.approval(&args) {
            match approver.approve(request).await {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny => {
                    return ToolOutcome::failure("The user denied this command.");
                }
                ApprovalDecision::Cancelled => {
                    return ToolOutcome::failure("Cancelled.");
                }
            }
        }

        let result = match tool.timeout() {
            Some(limit) => match tokio::time::timeout(limit, tool.execute(ctx, args)).await {
                Ok(result) => result,
                Err(_) => Err(ToolError::Timeout(limit)),
            },
            None => tool.execute(ctx, args).await,
        };

        match result {
            Ok(output) => ToolOutcome::success(output),
            Err(error) => ToolOutcome::failure(error.to_string()),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}

pub(crate) fn cap(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut capped: String = text.chars().take(max_chars).collect();
    capped.push_str("\n... [truncated]");
    capped
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{CancelHandle, ToolContext, Workspace};

    pub fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("oaklet-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    pub fn context(tag: &str) -> (ToolContext, PathBuf, CancelHandle) {
        let root = temp_root(tag);
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (handle, cancel) = CancelHandle::new();
        (ToolContext { workspace, cancel }, root, handle)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures_util::future::BoxFuture;
    use serde_json::{Value, json};

    use super::test_support::context;
    use super::{
        ApprovalDecision, ApprovalRequest, Approver, CancelHandle, Tool, ToolContext, ToolError,
        ToolRegistry, cap,
    };

    struct Allow;

    impl Approver for Allow {
        fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Allow })
        }
    }

    struct Deny;

    impl Approver for Deny {
        fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Deny })
        }
    }

    struct Stop;

    impl Approver for Stop {
        fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Cancelled })
        }
    }

    struct FakeTool {
        name: &'static str,
        needs_approval: bool,
        slow: bool,
        ran: Arc<AtomicBool>,
    }

    impl Tool for FakeTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn description(&self) -> &'static str {
            "fake"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }

        fn approval(&self, _args: &Value) -> Option<ApprovalRequest> {
            self.needs_approval.then(|| ApprovalRequest {
                tool: self.name,
                prompt: "Allow?".into(),
                detail: String::new(),
                args: json!({}),
            })
        }

        fn timeout(&self) -> Option<Duration> {
            self.slow.then_some(Duration::from_millis(10))
        }

        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
        ) -> BoxFuture<'a, Result<String, ToolError>> {
            self.ran.store(true, Ordering::SeqCst);
            if self.slow {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok("late".to_string())
                })
            } else {
                Box::pin(async { Ok("ran".to_string()) })
            }
        }
    }

    fn fake(
        name: &'static str,
        needs_approval: bool,
        slow: bool,
    ) -> (Box<dyn Tool>, Arc<AtomicBool>) {
        let ran = Arc::new(AtomicBool::new(false));
        (
            Box::new(FakeTool {
                name,
                needs_approval,
                slow,
                ran: ran.clone(),
            }),
            ran,
        )
    }

    #[test]
    fn caps_long_output_at_char_boundaries() {
        let text = "日本語".repeat(10);
        let capped = cap(text, 5);
        assert!(capped.starts_with("日本語日本"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn projects_every_tool_once() {
        let registry = ToolRegistry::new();
        let schemas = registry.schemas();
        let names: Vec<&str> = schemas
            .as_array()
            .unwrap()
            .iter()
            .map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "write_file",
                "edit_file",
                "list_dir",
                "run_command"
            ]
        );
    }

    #[test]
    fn summarizes_paths_and_commands() {
        let registry = ToolRegistry::new();
        assert_eq!(
            registry.summary("read_file", &json!({ "path": "src/main.rs" })),
            "src/main.rs"
        );
        assert_eq!(
            registry.summary("run_command", &json!({ "command": " cargo test " })),
            "cargo test"
        );
        assert_eq!(registry.summary("list_dir", &json!({})), ".");
        assert_eq!(registry.summary("nope", &json!({})), "");
    }

    #[tokio::test]
    async fn unknown_tool_is_a_failed_outcome() {
        let (ctx, root, _handle) = context("unknown");
        let outcome = ToolRegistry::new()
            .execute(&ctx, &Allow, "nope", json!({}))
            .await;
        assert!(!outcome.ok);
        assert!(outcome.output.contains("Unknown tool: nope"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn denied_approval_skips_execution() {
        let (tool, ran) = fake("risky", true, false);
        let registry = ToolRegistry { tools: vec![tool] };
        let (ctx, root, _handle) = context("denied");
        let outcome = registry.execute(&ctx, &Deny, "risky", json!({})).await;
        assert!(!ran.load(Ordering::SeqCst));
        assert!(!outcome.ok);
        assert_eq!(outcome.output, "The user denied this command.");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn cancelled_approval_skips_execution() {
        let (tool, ran) = fake("risky", true, false);
        let registry = ToolRegistry { tools: vec![tool] };
        let (ctx, root, _handle) = context("stopped");
        let outcome = registry.execute(&ctx, &Stop, "risky", json!({})).await;
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(outcome.output, "Cancelled.");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn timeout_is_a_failed_outcome() {
        let (tool, ran) = fake("slow", false, true);
        let registry = ToolRegistry { tools: vec![tool] };
        let (ctx, root, _handle) = context("timeout");
        let outcome = registry.execute(&ctx, &Allow, "slow", json!({})).await;
        assert!(ran.load(Ordering::SeqCst));
        assert!(outcome.output.starts_with("Timed out after"));
        std::fs::remove_dir_all(&root).ok();
    }

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
