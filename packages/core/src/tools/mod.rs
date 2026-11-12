//! Model-facing tool definitions and their execution pipeline.
//!
//! Each tool owns its schema, system-prompt guidance, approval policy, timeout,
//! and body. The registry projects the model-facing schemas; the pipeline owns
//! lookup, approval, deadlines, and result normalization. The agent loop never
//! matches on a tool name.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tauri::Manager;
use tokio::sync::watch;

mod fs;
mod likec4;
mod mcp;
mod patch;
mod shell;
pub(crate) mod skill;
pub(crate) mod web;
mod workspace;

pub use workspace::Workspace;

/// Tool output above this many characters keeps only its head and tail.
const TOOL_OUTPUT_THRESHOLD_CHARS: usize = 12_000;
/// Head kept verbatim from an oversized tool output.
const TOOL_OUTPUT_HEAD_CHARS: usize = 4_000;
/// Tail kept verbatim: errors and summaries usually land at the end.
const TOOL_OUTPUT_TAIL_CHARS: usize = 1_500;
/// Marker substituted for the removed middle.
const TOOL_OUTPUT_MARKER: &str = "\n\n[... tool output middle pruned ...]\n\n";

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
            output: prune_tool_output(output.into()),
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: prune_tool_output(message.into()),
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
    warnings: Vec<String>,
}

impl ToolRegistry {
    /// Tools that need host state beyond the workspace: MCP servers (including
    /// LikeC4) and, when an API key is configured, DeepSeek web search.
    pub async fn with_host_tools(app: &tauri::AppHandle, config: &crate::AppConfig) -> Self {
        let mut registry = Self::new();

        let workspace = PathBuf::from(&config.workspace);
        let home = app.path().home_dir().ok();
        let catalog = tauri::async_runtime::spawn_blocking(move || {
            skill::SkillCatalog::discover(&workspace, home.as_deref())
        })
        .await
        .unwrap_or_default();
        if catalog.has_model_skills() {
            registry
                .tools
                .push(Box::new(skill::LoadSkill::new(catalog)));
        }

        registry.tools.push(Box::new(likec4::LikeC4 {
            node: crate::architecture::find_node(),
            helper: crate::architecture::find_helper(app),
        }));
        let (tools, warnings) =
            mcp::discover(&config.mcp_servers, std::path::Path::new(&config.workspace)).await;
        registry.tools.extend(tools);
        registry.warnings = warnings;
        if !config.api_key.trim().is_empty() {
            registry.tools.push(Box::new(web::WebSearch::new(
                &config.api_key,
                &config.search_url,
            )));
        }
        registry
    }

    pub fn new() -> Self {
        Self {
            tools: vec![
                Box::new(fs::ReadFile),
                Box::new(fs::WriteFile),
                Box::new(fs::EditFile),
                Box::new(patch::ApplyPatch),
                Box::new(fs::ListDir),
                Box::new(shell::RunCommand),
            ],
            warnings: Vec::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(Box::as_ref)
    }

    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
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

    /// One prompt rule per tool that declares guidance, without duplicates.
    pub fn guidance(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        self.tools
            .iter()
            .map(|tool| tool.guidance())
            .filter(|text| !text.is_empty() && seen.insert(*text))
            .collect()
    }

    /// MCP servers that could not be started during discovery.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

/// Keep the head and tail of an oversized tool output.
///
/// The middle of a file dump or command log is the least useful part, while the
/// head carries the shape of the result and the tail carries errors and
/// summaries. Slicing is by character, so multi-byte text cannot be split.
fn prune_tool_output(text: String) -> String {
    let total = text.chars().count();
    if total <= TOOL_OUTPUT_THRESHOLD_CHARS {
        return text;
    }
    let head_end = TOOL_OUTPUT_HEAD_CHARS.min(total);
    let tail_start = total.saturating_sub(TOOL_OUTPUT_TAIL_CHARS).max(head_end);
    let mut pruned: String = text.chars().take(head_end).collect();
    pruned.push_str(TOOL_OUTPUT_MARKER);
    pruned.extend(text.chars().skip(tail_start));
    pruned
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
        ApprovalDecision, ApprovalRequest, Approver, CancelHandle, TOOL_OUTPUT_HEAD_CHARS,
        TOOL_OUTPUT_MARKER, TOOL_OUTPUT_TAIL_CHARS, TOOL_OUTPUT_THRESHOLD_CHARS, Tool, ToolContext,
        ToolError, ToolRegistry, cap, prune_tool_output,
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
        fn name(&self) -> &str {
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
                tool: self.name.to_string(),
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
    fn keeps_short_tool_output_verbatim() {
        let text = "x".repeat(TOOL_OUTPUT_THRESHOLD_CHARS);
        assert_eq!(prune_tool_output(text.clone()), text);
    }

    #[test]
    fn prunes_the_middle_of_a_long_tool_output() {
        let head = "h".repeat(TOOL_OUTPUT_HEAD_CHARS);
        let tail = "t".repeat(TOOL_OUTPUT_TAIL_CHARS);
        let text = format!("{head}{}{tail}", "m".repeat(TOOL_OUTPUT_THRESHOLD_CHARS));
        let pruned = prune_tool_output(text);

        assert!(pruned.starts_with(&head));
        assert!(pruned.ends_with(&tail));
        assert_eq!(pruned.matches(TOOL_OUTPUT_MARKER).count(), 1);
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn prunes_multibyte_output_without_splitting_characters() {
        let text = "日本語".repeat(10_000);
        let pruned = prune_tool_output(text);

        assert!(pruned.starts_with(&"日本語".repeat(TOOL_OUTPUT_HEAD_CHARS / 3)));
        assert!(pruned.ends_with(&"日本語".repeat(TOOL_OUTPUT_TAIL_CHARS / 3)));
        assert!(!pruned.contains('\u{fffd}'));
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn pruning_budget_fits_inside_the_threshold() {
        assert!(
            TOOL_OUTPUT_HEAD_CHARS + TOOL_OUTPUT_MARKER.chars().count() + TOOL_OUTPUT_TAIL_CHARS
                <= TOOL_OUTPUT_THRESHOLD_CHARS
        );
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
                "apply_patch",
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
        let registry = ToolRegistry {
            tools: vec![tool],
            warnings: Vec::new(),
        };
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
        let registry = ToolRegistry {
            tools: vec![tool],
            warnings: Vec::new(),
        };
        let (ctx, root, _handle) = context("stopped");
        let outcome = registry.execute(&ctx, &Stop, "risky", json!({})).await;
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(outcome.output, "Cancelled.");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn timeout_is_a_failed_outcome() {
        let (tool, ran) = fake("slow", false, true);
        let registry = ToolRegistry {
            tools: vec![tool],
            warnings: Vec::new(),
        };
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
