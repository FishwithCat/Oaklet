//! Executing the tool calls of one assistant turn.
//!
//! Consecutive read-only calls run concurrently; anything else is a barrier
//! that waits for the calls before it. Results are recorded in model order.

use std::path::PathBuf;

use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::ToolCall;
use super::approval::TauriApprover;
use super::messages::tool_message;
use super::parse_args;
use crate::chat::RunContext;
use crate::repeat::RepeatGuard;
use crate::tools::{Cancel, ToolContext, ToolImage, ToolOutcome, ToolRegistry, Workspace};

/// Read-only calls in one batch that may run at once. Bounded so a large
/// exploration batch cannot exhaust file descriptors.
const MAX_PARALLEL_READS: usize = 10;

/// Everything a batch of tool calls leaves behind for the next request.
#[derive(Default)]
pub(super) struct Batch {
    /// Repeat-guard reminders, appended after every tool result.
    pub reminders: Vec<String>,
    /// Images produced by tools, attached as one user turn.
    pub images: Vec<ToolImage>,
    /// Files that may reveal instructions not loaded yet.
    pub instruction_touches: Vec<PathBuf>,
    /// The run was cancelled part-way through the batch.
    pub cancelled: bool,
}

/// Run every call in model order, letting read-only runs overlap.
pub(super) async fn run(
    calls: &[ToolCall],
    tools: &ToolRegistry,
    tool_context: &ToolContext,
    ctx: &RunContext,
    cancel: &Cancel,
    guard: &mut RepeatGuard,
    messages: &mut Vec<Value>,
) -> Batch {
    ToolRun {
        tools,
        tool_context,
        ctx,
        cancel,
    }
    .run(calls, guard, messages)
    .await
}

/// The run-scoped handles every tool call needs.
struct ToolRun<'a> {
    tools: &'a ToolRegistry,
    tool_context: &'a ToolContext,
    ctx: &'a RunContext,
    cancel: &'a Cancel,
}

impl ToolRun<'_> {
    async fn run(
        &self,
        calls: &[ToolCall],
        guard: &mut RepeatGuard,
        messages: &mut Vec<Value>,
    ) -> Batch {
        let mut batch = Batch::default();
        let mut index = 0;
        while index < calls.len() {
            if self.cancel.is_cancelled() {
                batch.cancelled = true;
                return batch;
            }

            // Consecutive read-only calls overlap; anything else is a barrier.
            if self.tools.is_concurrency_safe(&calls[index].name) {
                let start = index;
                while index < calls.len() && self.tools.is_concurrency_safe(&calls[index].name) {
                    index += 1;
                }
                self.reads(&calls[start..index], guard, messages, &mut batch)
                    .await;
                continue;
            }

            self.one(&calls[index], guard, messages, &mut batch).await;
            index += 1;
            if self.cancel.is_cancelled() {
                batch.cancelled = true;
                return batch;
            }
        }
        batch
    }

    /// Prepare every read first, then run them concurrently.
    async fn reads(
        &self,
        group: &[ToolCall],
        guard: &mut RepeatGuard,
        messages: &mut Vec<Value>,
        batch: &mut Batch,
    ) {
        let mut prepared = Vec::with_capacity(group.len());
        for call in group {
            let args = parse_args(&call.arguments);
            let summary = match &args {
                Ok(args) => self.tools.summary(&call.name, args),
                Err(_) => String::new(),
            };
            let diffs = match &args {
                Ok(args) => self
                    .tools
                    .preview_diffs(self.tool_context, &call.name, args),
                Err(_) => Vec::new(),
            };
            self.ctx.tool_start(&call.id, &call.name, &summary, &diffs);
            if let Some(reminder) = observe(guard, call, &args) {
                batch.reminders.push(reminder);
            }
            let touched = match &args {
                Ok(args) => {
                    touched_instruction_path(&self.tool_context.workspace, &call.name, args)
                }
                Err(_) => None,
            };
            prepared.push((args, touched));
        }

        let executions: Vec<BoxFuture<'_, (&ToolCall, ToolOutcome, Option<PathBuf>)>> = group
            .iter()
            .zip(prepared)
            .map(|(call, (args, touched))| {
                let tools = self.tools;
                let tool_context = self.tool_context;
                let ctx = self.ctx;
                let cancel = self.cancel;
                Box::pin(async move {
                    let outcome = match args {
                        Err(message) => ToolOutcome::failure(message),
                        Ok(args) => {
                            let approver = TauriApprover {
                                ctx,
                                call_id: &call.id,
                                workspace: tool_context.workspace.root(),
                                cancel,
                            };
                            tools
                                .execute(tool_context, &approver, &call.name, &call.id, args)
                                .await
                        }
                    };
                    (call, outcome, touched)
                }) as BoxFuture<'_, (&ToolCall, ToolOutcome, Option<PathBuf>)>
            })
            .collect();

        let mut outcomes = read_group(executions);
        while let Some((call, outcome, touched)) = outcomes.next().await {
            record(self.ctx, call, outcome, touched, messages, batch);
        }
    }

    /// A call that must wait for everything before it to finish.
    async fn one(
        &self,
        call: &ToolCall,
        guard: &mut RepeatGuard,
        messages: &mut Vec<Value>,
        batch: &mut Batch,
    ) {
        let args = match parse_args(&call.arguments) {
            Ok(args) => args,
            Err(message) => {
                self.ctx.tool_start(&call.id, &call.name, "", &[]);
                self.ctx.tool_result(&call.id, false, &message, &[]);
                if let Some(reminder) = observe(guard, call, &Err(message.clone())) {
                    batch.reminders.push(reminder);
                }
                messages.push(tool_message(&call.id, message));
                return;
            }
        };

        let summary = self.tools.summary(&call.name, &args);
        let diffs = self
            .tools
            .preview_diffs(self.tool_context, &call.name, &args);
        self.ctx.tool_start(&call.id, &call.name, &summary, &diffs);
        if let Some(reminder) = guard.observe(&call.name, &args) {
            batch.reminders.push(reminder);
        }
        let touched = touched_instruction_path(&self.tool_context.workspace, &call.name, &args);

        let approver = TauriApprover {
            ctx: self.ctx,
            call_id: &call.id,
            workspace: self.tool_context.workspace.root(),
            cancel: self.cancel,
        };
        let outcome = self
            .tools
            .execute(self.tool_context, &approver, &call.name, &call.id, args)
            .await;
        record(self.ctx, call, outcome, touched, messages, batch);
    }
}

/// Feed one call to the repeat guard, using the raw text when it did not parse.
fn observe(
    guard: &mut RepeatGuard,
    call: &ToolCall,
    args: &Result<Value, String>,
) -> Option<String> {
    match args {
        Ok(args) => guard.observe(&call.name, args),
        Err(_) => guard.observe(&call.name, &Value::String(call.arguments.clone())),
    }
}

fn record(
    ctx: &RunContext,
    call: &ToolCall,
    outcome: ToolOutcome,
    touched: Option<PathBuf>,
    messages: &mut Vec<Value>,
    batch: &mut Batch,
) {
    ctx.tool_result(&call.id, outcome.ok, &outcome.output, &outcome.diffs);
    if outcome.ok
        && let Some(touched) = touched
    {
        batch.instruction_touches.push(touched);
    }
    messages.push(tool_message(&call.id, outcome.output));
    batch.images.extend(outcome.images);
}

/// Run one consecutive group of read-only calls concurrently.
///
/// `buffered` keeps at most [`MAX_PARALLEL_READS`] bodies in flight and yields
/// their outcomes in model order, so a later read that finishes first still
/// lands after the earlier tool result in the transcript.
fn read_group<'a>(
    executions: Vec<BoxFuture<'a, (&'a ToolCall, ToolOutcome, Option<PathBuf>)>>,
) -> impl futures_util::Stream<Item = (&'a ToolCall, ToolOutcome, Option<PathBuf>)> + 'a {
    futures_util::stream::iter(executions).buffered(MAX_PARALLEL_READS)
}

/// File paths that can reveal instructions in a directory the run has not
/// scanned yet. `apply_patch` is omitted; it parses several paths from one
/// patch, and a read in the same directory usually precedes it.
fn touched_instruction_path(workspace: &Workspace, tool: &str, args: &Value) -> Option<PathBuf> {
    if !matches!(tool, "read_file" | "write_file" | "edit_file") {
        return None;
    }
    let path = args.get("path").and_then(Value::as_str)?;
    workspace.resolve(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn concurrent_reads_overlap_and_keep_model_order() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = [
            ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
            ToolCall {
                id: "c2".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
        ];
        let executions: Vec<BoxFuture<'_, (&ToolCall, ToolOutcome, Option<PathBuf>)>> = calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let active = active.clone();
                let peak = peak.clone();
                Box::pin(async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    // The first read is slower, so ordering cannot come from
                    // completion order.
                    tokio::time::sleep(Duration::from_millis(if index == 0 { 50 } else { 1 }))
                        .await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    (
                        call,
                        ToolOutcome {
                            ok: true,
                            output: format!("read {index}"),
                            images: Vec::new(),
                            diffs: Vec::new(),
                        },
                        None,
                    )
                }) as BoxFuture<'_, (&ToolCall, ToolOutcome, Option<PathBuf>)>
            })
            .collect();

        let mut outcomes = read_group(executions);
        let mut results = Vec::new();
        while let Some(outcome) = outcomes.next().await {
            results.push(outcome);
        }

        assert_eq!(peak.load(Ordering::SeqCst), 2);
        let ids: Vec<&str> = results
            .iter()
            .map(|(call, _, _)| call.id.as_str())
            .collect();
        assert_eq!(ids, ["c1", "c2"]);
        let labels: Vec<&str> = results
            .iter()
            .map(|(_, outcome, _)| outcome.output.as_str())
            .collect();
        assert_eq!(labels, ["read 0", "read 1"]);
    }
}
