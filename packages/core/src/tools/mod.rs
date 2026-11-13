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

use crate::spill;

mod diff;
mod fs;
pub(crate) mod history;
mod image;
pub(crate) mod jobs;
mod likec4;
mod mcp;
mod patch;
mod search;
mod shell;
pub(crate) mod skill;
pub(crate) mod web;
mod web_fetch;
mod workspace;

pub use diff::FileDiff;
pub use workspace::Workspace;

/// Tool output above this many characters keeps only its head and tail.
const TOOL_OUTPUT_THRESHOLD_CHARS: usize = 12_000;
/// Head kept verbatim from an oversized tool output.
const TOOL_OUTPUT_HEAD_CHARS: usize = 4_000;
/// Tail kept verbatim: errors and summaries usually land at the end.
const TOOL_OUTPUT_TAIL_CHARS: usize = 1_500;
/// Marker substituted for the removed middle.
const TOOL_OUTPUT_MARKER: &str = "\n\n[... tool output middle pruned ...]\n\n";
/// Reading a spill file must not spill again, or each page would be another file.
const TOOL_SPILL_SKIP: &str = "read_file";
/// The complete marker written when a tool output was spilled. Matching all of
/// it keeps ordinary log text that mentions `saved to:` from being mistaken for
/// a spill pointer.
const SPILL_MARKER_HEAD: &str = "[... tool output middle pruned; full output (";
const SPILL_MARKER_POINTER: &str = ") saved to: ";
const SPILL_MARKER_TAIL: &str = ". Use read_file with offset and limit to page through it.]";
const SPILL_RECOVERY_HEAD: &str =
    "[... tool output trimmed for context recovery; full output saved to: ";
/// The spill writer's workspace-relative paths are far shorter than this.
const SPILL_POINTER_MAX_CHARS: usize = 512;

/// Extract the path from a complete, Oaklet-generated spill marker.
pub(crate) fn spill_pointer(text: &str) -> Option<&str> {
    for head in [SPILL_MARKER_HEAD, SPILL_RECOVERY_HEAD] {
        for (start, _) in text.match_indices(head) {
            let rest = &text[start + head.len()..];
            let after_pointer = if head == SPILL_MARKER_HEAD {
                let Some((_, after_pointer)) = rest.split_once(SPILL_MARKER_POINTER) else {
                    continue;
                };
                after_pointer
            } else {
                rest
            };
            let Some((path, _)) = after_pointer.split_once(SPILL_MARKER_TAIL) else {
                continue;
            };
            if (path.starts_with(".oaklet/spill/") || path.starts_with(".oaklet\\spill\\"))
                && path.chars().count() <= SPILL_POINTER_MAX_CHARS
            {
                return Some(path);
            }
        }
    }
    None
}

/// One image returned by a tool. `data_url` is a complete `data:` URL so the
/// agent loop can attach it to the next model request without another read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolImage {
    pub data_url: String,
}

/// Result of one tool call. Most tools return text only; a few attach images
/// the agent loop forwards as a separate user message, and file-mutation tools
/// attach display-only diffs the chat UI renders.
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub text: String,
    pub images: Vec<ToolImage>,
    pub diffs: Vec<FileDiff>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            diffs: Vec::new(),
        }
    }

    pub fn with_image(text: impl Into<String>, data_url: String) -> Self {
        Self {
            text: text.into(),
            images: vec![ToolImage { data_url }],
            diffs: Vec::new(),
        }
    }

    pub fn with_diffs(text: impl Into<String>, diffs: Vec<FileDiff>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            diffs,
        }
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl PartialEq<&str> for ToolOutput {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<str> for ToolOutput {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

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

/// Normalized result handed back to the agent loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
    /// Images returned by the call; the agent loop attaches them after the
    /// batch's tool messages so every provider sees a normal image user turn.
    pub images: Vec<ToolImage>,
    /// Applied diffs for the chat UI; empty for tools that do not mutate files.
    pub diffs: Vec<FileDiff>,
}

impl ToolOutcome {
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: message.into(),
            images: Vec::new(),
            diffs: Vec::new(),
        }
    }
}

/// Per-run services available to every tool.
pub struct ToolContext {
    pub workspace: Workspace,
    pub cancel: Cancel,
    /// Background jobs owned by the app process, shared across runs.
    pub jobs: std::sync::Arc<jobs::JobRegistry>,
    /// Session run that owns spill files and, later, recall metadata.
    pub run_id: String,
}

impl ToolContext {
    /// Save oversized output before trimming it, so the full text survives in
    /// a file the model can page through. A failed write keeps the old marker.
    fn bound_output(&self, tool: &str, call_id: &str, text: String) -> String {
        if text.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS {
            return text;
        }
        let spilled = (tool != TOOL_SPILL_SKIP)
            .then(|| {
                spill::save(
                    self.workspace.root(),
                    &self.run_id,
                    &format!("{tool}-{call_id}"),
                    &text,
                )
            })
            .flatten();
        let marker = match &spilled {
            Some(spilled) => format!(
                "\n\n{SPILL_MARKER_HEAD}{} lines, {} bytes{SPILL_MARKER_POINTER}{}{SPILL_MARKER_TAIL}\n\n",
                spilled.lines,
                spilled.bytes,
                spilled.relative.display()
            ),
            None => TOOL_OUTPUT_MARKER.to_string(),
        };
        prune_tool_output(text, &marker)
    }
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
    /// LikeC4) and, when a search key is configured, web search.
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
        let search_key = if config.search_api_key.trim().is_empty() {
            config.api_key.trim()
        } else {
            config.search_api_key.trim()
        };
        if !search_key.is_empty() {
            registry.tools.push(Box::new(web::WebSearch::new(
                search_key,
                &config.search_url,
            )));
        }
        registry.tools.push(Box::new(web_fetch::WebFetch));
        registry
    }

    pub fn new() -> Self {
        Self {
            tools: vec![
                Box::new(fs::ReadFile),
                Box::new(image::ReadImage),
                Box::new(fs::WriteFile),
                Box::new(fs::EditFile),
                Box::new(patch::ApplyPatch),
                Box::new(fs::ListDir),
                Box::new(search::Glob),
                Box::new(search::Grep),
                Box::new(shell::RunCommand),
                Box::new(jobs::JobList),
                Box::new(jobs::JobOutput),
                Box::new(jobs::JobKill),
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

    pub fn push(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
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

    /// Pending-call diff preview for tools that edit files.
    pub fn preview_diffs(&self, ctx: &ToolContext, name: &str, args: &Value) -> Vec<FileDiff> {
        self.get(name)
            .map(|tool| tool.preview(ctx, args))
            .unwrap_or_default()
    }

    /// Whether a tool may join a concurrent read group. Unknown and undeclared
    /// tools stay exclusive.
    pub fn is_concurrency_safe(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|tool| tool.is_concurrency_safe())
    }

    /// Look up, ask, run under a deadline, and normalize one call.
    pub async fn execute(
        &self,
        ctx: &ToolContext,
        approver: &dyn Approver,
        name: &str,
        call_id: &str,
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
            Ok(output) => ToolOutcome {
                ok: true,
                output: ctx.bound_output(name, call_id, output.text),
                images: output.images,
                diffs: output.diffs,
            },
            Err(error) => ToolOutcome::failure(ctx.bound_output(name, call_id, error.to_string())),
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
/// summaries. Slicing is by character, so multi-byte text cannot be split. The
/// marker is budgeted too, so a long spill path cannot grow the result past the
/// threshold.
fn prune_tool_output(text: String, marker: &str) -> String {
    let total = text.chars().count();
    if total <= TOOL_OUTPUT_THRESHOLD_CHARS {
        return text;
    }
    let budget = TOOL_OUTPUT_THRESHOLD_CHARS.saturating_sub(marker.chars().count());
    let head_end = TOOL_OUTPUT_HEAD_CHARS.min(total).min(budget);
    let tail_count = TOOL_OUTPUT_TAIL_CHARS
        .min(total - head_end)
        .min(budget - head_end);
    let mut pruned: String = text.chars().take(head_end).collect();
    pruned.push_str(marker);
    pruned.extend(text.chars().skip(total - tail_count));
    pruned
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{CancelHandle, ToolContext, Workspace, jobs};

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
        (
            ToolContext {
                workspace,
                cancel,
                jobs: std::sync::Arc::new(jobs::JobRegistry::new()),
                run_id: "run-test".into(),
            },
            root,
            handle,
        )
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
        ApprovalDecision, ApprovalRequest, Approver, CancelHandle, SPILL_MARKER_HEAD,
        SPILL_MARKER_POINTER, SPILL_MARKER_TAIL, TOOL_OUTPUT_HEAD_CHARS, TOOL_OUTPUT_MARKER,
        TOOL_OUTPUT_TAIL_CHARS, TOOL_OUTPUT_THRESHOLD_CHARS, Tool, ToolContext, ToolError,
        ToolOutput, ToolRegistry, cap, prune_tool_output, spill_pointer,
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
        ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            self.ran.store(true, Ordering::SeqCst);
            if self.slow {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(ToolOutput::text("late"))
                })
            } else {
                Box::pin(async { Ok(ToolOutput::text("ran")) })
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

    struct Wordy {
        name: &'static str,
        output: String,
    }

    impl Tool for Wordy {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &'static str {
            "wordy"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }

        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
        ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            let output = self.output.clone();
            Box::pin(async move { Ok(ToolOutput::text(output)) })
        }
    }

    fn wordy(name: &'static str, output: String) -> ToolRegistry {
        ToolRegistry {
            tools: vec![Box::new(Wordy { name, output })],
            warnings: Vec::new(),
        }
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
        assert_eq!(prune_tool_output(text.clone(), TOOL_OUTPUT_MARKER), text);
    }

    #[test]
    fn prunes_the_middle_of_a_long_tool_output() {
        let head = "h".repeat(TOOL_OUTPUT_HEAD_CHARS);
        let tail = "t".repeat(TOOL_OUTPUT_TAIL_CHARS);
        let text = format!("{head}{}{tail}", "m".repeat(TOOL_OUTPUT_THRESHOLD_CHARS));
        let pruned = prune_tool_output(text, TOOL_OUTPUT_MARKER);

        assert!(pruned.starts_with(&head));
        assert!(pruned.ends_with(&tail));
        assert_eq!(pruned.matches(TOOL_OUTPUT_MARKER).count(), 1);
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn prunes_multibyte_output_without_splitting_characters() {
        let text = "日本語".repeat(10_000);
        let pruned = prune_tool_output(text, TOOL_OUTPUT_MARKER);

        assert!(pruned.starts_with(&"日本語".repeat(TOOL_OUTPUT_HEAD_CHARS / 3)));
        assert!(pruned.ends_with(&"日本語".repeat(TOOL_OUTPUT_TAIL_CHARS / 3)));
        assert!(!pruned.contains('\u{fffd}'));
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn a_long_marker_stays_inside_the_pruning_budget() {
        let text = "x".repeat(TOOL_OUTPUT_THRESHOLD_CHARS * 2);
        let marker = format!("[saved to {}]", "p".repeat(TOOL_OUTPUT_THRESHOLD_CHARS / 2));
        let pruned = prune_tool_output(text, &marker);

        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
        assert!(pruned.contains(&marker));
    }

    #[tokio::test]
    async fn oversized_tool_output_is_spilled_with_a_readable_marker() {
        let (ctx, root, _handle) = context("spill-output");
        let full = format!(
            "HEAD\n{}\nMIDDLE-TOKEN\n{}\nTAIL",
            "a".repeat(6_000),
            "b".repeat(6_000)
        );
        let registry = wordy("wordy", full.clone());
        let outcome = registry
            .execute(&ctx, &Allow, "wordy", "call-7", json!({}))
            .await;

        let relative = std::path::Path::new(".oaklet/spill/run-test/wordy-call-7.txt");
        assert_eq!(
            std::fs::read_to_string(root.join(relative)).unwrap(),
            full,
            "spill file keeps the complete output"
        );
        assert!(outcome.ok);
        assert!(outcome.output.contains(&relative.display().to_string()));
        assert!(
            outcome
                .output
                .contains(&format!("{} lines", full.lines().count()))
        );
        assert!(outcome.output.contains(&format!("{} bytes", full.len())));
        assert!(
            outcome
                .output
                .contains("Use read_file with offset and limit")
        );
        assert!(!outcome.output.contains("MIDDLE-TOKEN"));
        assert!(outcome.output.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);

        // The marker's instruction works: paging with offset/limit recovers the
        // part the model could not see inline.
        let read = ToolRegistry::new()
            .execute(
                &ctx,
                &Allow,
                "read_file",
                "call-8",
                json!({ "path": relative.to_str().unwrap(), "offset": 3, "limit": 2 }),
            )
            .await;
        assert!(read.ok);
        assert!(read.output.contains("MIDDLE-TOKEN"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn read_file_output_is_not_spilled_again() {
        let (ctx, root, _handle) = context("spill-read");
        let registry = wordy("read_file", "r".repeat(TOOL_OUTPUT_THRESHOLD_CHARS + 1));
        let outcome = registry
            .execute(&ctx, &Allow, "read_file", "call-1", json!({}))
            .await;

        assert!(outcome.output.contains(TOOL_OUTPUT_MARKER));
        assert!(!root.join(".oaklet/spill").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn an_unwritable_spill_falls_back_to_the_plain_marker() {
        let (ctx, root, _handle) = context("spill-fail");
        std::fs::write(root.join(".oaklet"), "not a directory").unwrap();
        let registry = wordy("wordy", "w".repeat(TOOL_OUTPUT_THRESHOLD_CHARS + 1));
        let outcome = registry
            .execute(&ctx, &Allow, "wordy", "call-1", json!({}))
            .await;

        assert!(outcome.output.contains(TOOL_OUTPUT_MARKER));
        assert!(!outcome.output.contains("saved to:"));
        assert!(outcome.output.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn spill_pointer_ignores_saved_to_text_before_the_marker() {
        let path = ".oaklet/spill/run-1/out.txt";
        let text = format!(
            "artifact saved to: build/report.html\n\n{SPILL_MARKER_HEAD}900 lines, 20000 bytes{SPILL_MARKER_POINTER}{path}{SPILL_MARKER_TAIL}"
        );

        assert_eq!(spill_pointer(&text), Some(path));

        let not_a_spill = format!(
            "{SPILL_MARKER_HEAD}1 lines, 2 bytes{SPILL_MARKER_POINTER}build/report.html{SPILL_MARKER_TAIL}"
        );
        assert_eq!(spill_pointer(&not_a_spill), None);
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
                "read_image",
                "write_file",
                "edit_file",
                "apply_patch",
                "list_dir",
                "glob",
                "grep",
                "run_command",
                "job_list",
                "job_output",
                "job_kill"
            ]
        );
    }

    #[test]
    fn only_read_only_tools_are_concurrency_safe() {
        let registry = ToolRegistry::new();
        assert!(registry.is_concurrency_safe("read_file"));
        assert!(registry.is_concurrency_safe("read_image"));
        assert!(registry.is_concurrency_safe("list_dir"));
        assert!(registry.is_concurrency_safe("glob"));
        assert!(registry.is_concurrency_safe("grep"));
        assert!(registry.is_concurrency_safe("job_list"));
        for name in [
            "write_file",
            "edit_file",
            "apply_patch",
            "run_command",
            "job_output",
            "job_kill",
            "nope",
        ] {
            assert!(!registry.is_concurrency_safe(name), "{name}");
        }
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

    #[test]
    fn previews_pending_file_mutations() {
        let (ctx, root, _handle) = context("preview");
        std::fs::write(root.join("notes.txt"), "old\n").unwrap();
        let registry = ToolRegistry::new();

        let write = registry.preview_diffs(
            &ctx,
            "write_file",
            &json!({ "path": "notes.txt", "content": "new\n" }),
        );
        assert_eq!(write.len(), 1);
        assert_eq!(write[0].old_text.as_deref(), Some("old"));
        assert_eq!(write[0].new_text, "new");

        let edit = registry.preview_diffs(
            &ctx,
            "edit_file",
            &json!({ "path": "notes.txt", "old_string": "old", "new_string": "new" }),
        );
        assert_eq!(edit[0].old_text.as_deref(), Some("old"));
        assert_eq!(edit[0].new_text, "new");

        let patch = registry.preview_diffs(
            &ctx,
            "apply_patch",
            &json!({ "input": "*** Begin Patch\n*** Update File: notes.txt\n@@\n-old\n+new\n*** End Patch" }),
        );
        assert_eq!(patch[0].path, "notes.txt");
        assert_eq!(patch[0].old_text.as_deref(), Some("old"));
        assert_eq!(patch[0].new_text, "new");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn unknown_tool_is_a_failed_outcome() {
        let (ctx, root, _handle) = context("unknown");
        let outcome = ToolRegistry::new()
            .execute(&ctx, &Allow, "nope", "call-1", json!({}))
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
        let outcome = registry
            .execute(&ctx, &Deny, "risky", "call-1", json!({}))
            .await;
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
        let outcome = registry
            .execute(&ctx, &Stop, "risky", "call-1", json!({}))
            .await;
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
        let outcome = registry
            .execute(&ctx, &Allow, "slow", "call-1", json!({}))
            .await;
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
