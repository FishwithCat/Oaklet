use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::tools::output::{TOOL_OUTPUT_MARKER, TOOL_OUTPUT_THRESHOLD_CHARS};
use crate::tools::test_support::{Allow, context};
use crate::tools::{
    Access, ApprovalDecision, ApprovalRequest, Approver, ArchitectureImpact, Assessment, Intent,
    ProcessOutcome, Tool, ToolContext, ToolError, ToolOutput, spill_pointer,
};

use super::ToolRegistry;

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

fn fake(name: &'static str, needs_approval: bool, slow: bool) -> (Box<dyn Tool>, Arc<AtomicBool>) {
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
        access: Access::execute(),
        planning: None,
        approved_plan: None,
    }
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

    let path = spill_pointer(&outcome.output).unwrap().to_string();
    assert!(std::path::Path::new(&path).starts_with(&ctx.spill_root));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        full,
        "spill file keeps the complete output"
    );
    assert!(outcome.ok);
    assert!(outcome.output.contains(&path));
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
            json!({ "path": path, "offset": 3, "limit": 2 }),
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
    assert!(!ctx.spill_root.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn an_unwritable_spill_falls_back_to_the_plain_marker() {
    let (ctx, root, _handle) = context("spill-fail");
    std::fs::write(&ctx.spill_root, "not a directory").unwrap();
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
            "file_outline",
            "read_image",
            "write_file",
            "edit_file",
            "apply_patch",
            "list_dir",
            "glob",
            "grep",
            "structural_search",
            "run_command",
            "job_list",
            "job_output",
            "job_kill"
        ]
    );
}

#[test]
fn plan_tools_appear_only_in_planning_runs() {
    let mut registry = ToolRegistry::new();
    let names = |registry: &ToolRegistry| -> Vec<String> {
        registry
            .schemas()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    assert!(!names(&registry).contains(&"assess_change".to_string()));
    assert!(!names(&registry).contains(&"present_plan".to_string()));

    registry.set_access(Access::planning());
    assert!(names(&registry).contains(&"assess_change".to_string()));
    assert!(names(&registry).contains(&"present_plan".to_string()));
}

#[test]
fn only_read_only_tools_are_concurrency_safe() {
    let registry = ToolRegistry::new();
    assert!(registry.is_concurrency_safe("read_file"));
    assert!(registry.is_concurrency_safe("file_outline"));
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

#[tokio::test]
async fn planning_gate_hides_writes_and_never_opens_them() {
    let (ctx, root, _handle) = context("planning-gate");
    let mut registry = ToolRegistry::new();
    registry.set_access(Access::planning());

    let names: Vec<String> = registry
        .schemas()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|schema| {
            schema
                .pointer("/function/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    for allowed in [
        "assess_change",
        "read_file",
        "file_outline",
        "read_image",
        "list_dir",
        "glob",
        "grep",
    ] {
        assert!(names.iter().any(|name| name == allowed), "{allowed}");
    }
    for denied in [
        "write_file",
        "edit_file",
        "apply_patch",
        "run_command",
        "job_list",
        "job_output",
        "job_kill",
    ] {
        assert!(!names.iter().any(|name| name == denied), "{denied}");
    }

    let outcome = registry
        .execute(
            &ctx,
            &Allow,
            "write_file",
            "call-1",
            json!({ "path": "notes.txt", "content": "leak" }),
        )
        .await;
    assert!(!outcome.ok);
    assert!(outcome.output.contains("read-only architecture planning"));
    assert!(!root.join("notes.txt").exists());

    registry.access().assess(Assessment {
        intent: Intent::Implement,
        impact: ArchitectureImpact::None,
    });
    let schemas = registry.schemas();
    let names: Vec<&str> = schemas
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|schema| schema.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(!names.contains(&"write_file"));
    let outcome = registry
        .execute(
            &ctx,
            &Allow,
            "write_file",
            "call-2",
            json!({ "path": "notes.txt", "content": "saved" }),
        )
        .await;
    assert!(!outcome.ok);
    assert!(!root.join("notes.txt").exists());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_locked_gate_points_at_the_pending_review() {
    let (ctx, root, _handle) = context("locked-gate");
    let mut registry = ToolRegistry::new();
    registry.set_access(Access::locked_read_only());
    let outcome = registry
        .execute(
            &ctx,
            &Allow,
            "run_command",
            "call-1",
            json!({ "command": "ls" }),
        )
        .await;
    assert!(!outcome.ok);
    assert!(outcome.output.contains("waiting for review"));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn newly_discovered_architecture_work_revokes_writes_for_an_approved_run() {
    let (mut ctx, root, _handle) = context("approved-scope-change");
    ctx.access = Access::approved();
    let mut registry = ToolRegistry::new();
    registry.set_access(ctx.access.clone());
    let assessment = registry
        .execute(
            &ctx,
            &Allow,
            "assess_change",
            "reassess",
            json!({ "intent": "revise", "architecture_impact": "confirmed" }),
        )
        .await;
    assert!(assessment.ok);
    assert!(assessment.output.contains("Execution was paused"));
    let write = registry
        .execute(
            &ctx,
            &Allow,
            "write_file",
            "write",
            json!({ "path": "out-of-scope.txt", "content": "denied" }),
        )
        .await;
    assert!(!write.ok);
    assert!(!root.join("out-of-scope.txt").exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn summarizes_paths_and_commands() {
    let registry = ToolRegistry::new();
    assert_eq!(
        registry.summary("read_file", &json!({ "path": "src/main.rs" })),
        "src/main.rs"
    );
    assert_eq!(
        registry.summary(
            "read_file",
            &json!({ "path": "src/main.rs", "offset": 40, "limit": 20 })
        ),
        "src/main.rs:40-59"
    );
    assert_eq!(
        registry.summary("read_file", &json!({ "path": "src/main.rs", "offset": 40 })),
        "src/main.rs:40-"
    );
    assert_eq!(
        registry.summary("read_file", &json!({ "path": "src/main.rs", "limit": 20 })),
        "src/main.rs:1-20"
    );
    assert_eq!(
        registry.summary(
            "read_file",
            &json!({ "path": "src/main.rs", "offset": 0, "limit": 0 })
        ),
        "src/main.rs:1-1"
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
        access: Access::execute(),
        planning: None,
        approved_plan: None,
    };
    let (ctx, root, _handle) = context("denied");
    let outcome = registry
        .execute(&ctx, &Deny, "risky", "call-1", json!({}))
        .await;
    assert!(!ran.load(Ordering::SeqCst));
    assert!(!outcome.ok);
    assert_eq!(outcome.output, "The user denied this command.");
    assert_eq!(outcome.outcome, Some(ProcessOutcome::Denied));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn cancelled_approval_skips_execution() {
    let (tool, ran) = fake("risky", true, false);
    let registry = ToolRegistry {
        tools: vec![tool],
        warnings: Vec::new(),
        access: Access::execute(),
        planning: None,
        approved_plan: None,
    };
    let (ctx, root, _handle) = context("stopped");
    let outcome = registry
        .execute(&ctx, &Stop, "risky", "call-1", json!({}))
        .await;
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(outcome.output, "Cancelled.");
    assert_eq!(outcome.outcome, Some(ProcessOutcome::Cancelled));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn timeout_is_a_failed_outcome() {
    let (tool, ran) = fake("slow", false, true);
    let registry = ToolRegistry {
        tools: vec![tool],
        warnings: Vec::new(),
        access: Access::execute(),
        planning: None,
        approved_plan: None,
    };
    let (ctx, root, _handle) = context("timeout");
    let outcome = registry
        .execute(&ctx, &Allow, "slow", "call-1", json!({}))
        .await;
    assert!(ran.load(Ordering::SeqCst));
    assert!(outcome.output.starts_with("Timed out after"));
    assert_eq!(outcome.outcome, Some(ProcessOutcome::TimedOut));
    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_nonzero_exit_propagates_as_a_failed_tool_result() {
    let (ctx, root, _handle) = context("real-exit");
    let outcome = ToolRegistry::new()
        .execute(
            &ctx,
            &Allow,
            "run_command",
            "call-1",
            json!({ "command": "printf 'stdout'; printf 'stderr' >&2; exit 4" }),
        )
        .await;
    assert!(!outcome.ok);
    assert_eq!(outcome.outcome, Some(ProcessOutcome::Exited { code: 4 }));
    assert!(outcome.output.starts_with("Exit code 4."));
    assert!(outcome.output.contains("stdout"));
    assert!(outcome.output.contains("stderr"));
    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_successful_process_keeps_the_tool_result_ok() {
    let (ctx, root, _handle) = context("real-success");
    let outcome = ToolRegistry::new()
        .execute(
            &ctx,
            &Allow,
            "run_command",
            "call-1",
            json!({ "command": "printf 'done'" }),
        )
        .await;
    assert!(outcome.ok);
    assert_eq!(outcome.outcome, Some(ProcessOutcome::Succeeded));
    assert!(outcome.output.contains("done"));
    std::fs::remove_dir_all(&root).ok();
}
