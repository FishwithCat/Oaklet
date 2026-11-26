//! Apply the checked plan in order, reporting partial completion.

use super::apply::{apply_hunks, mismatch_message, normalize_lf, restore_line_endings, uses_crlf};
use super::parse::{PatchHunk, PatchOperation};
use super::plan::{Kind, ResolvedPatch};
use crate::tools::fs::write_atomic;
use crate::tools::{Cancel, ToolError};

fn updated_text(operation: &ResolvedPatch, hunks: &[PatchHunk]) -> Result<String, String> {
    let text = std::fs::read_to_string(&operation.source)
        .map_err(|error| format!("cannot read \"{}\": {error}", operation.source_display))?;
    let crlf = uses_crlf(&text);
    let text = normalize_lf(&text);
    if text.contains('\0') {
        return Err(format!(
            "cannot patch \"{}\": binary file",
            operation.source_display
        ));
    }
    let patched = apply_hunks(&text, hunks)
        .map_err(|failure| mismatch_message(&operation.source_display, &failure))?;
    Ok(restore_line_endings(crlf, patched))
}

pub(super) fn execute_one(operation: &ResolvedPatch) -> Result<(), String> {
    match &operation.operation {
        PatchOperation::Add { content, .. } => write_atomic(&operation.source, content, true)
            .map_err(|error| format!("cannot write \"{}\": {error}", operation.source_display)),
        PatchOperation::Update {
            hunks,
            move_to: Some(_),
            ..
        } => {
            let after = updated_text(operation, hunks)?;
            let Some((destination, destination_display)) = &operation.destination else {
                return Err("Move destination is missing.".into());
            };
            write_atomic(destination, &after, true)
                .map_err(|error| format!("cannot write \"{destination_display}\": {error}"))?;
            std::fs::remove_file(&operation.source).map_err(|error| {
                format!(
                    "cannot move \"{}\" to \"{destination_display}\": destination was written but the source could not be removed: {error}",
                    operation.source_display
                )
            })
        }
        PatchOperation::Update { hunks, .. } => {
            let after = updated_text(operation, hunks)?;
            write_atomic(&operation.source, &after, false)
                .map_err(|error| format!("cannot write \"{}\": {error}", operation.source_display))
        }
        PatchOperation::Delete { .. } => std::fs::remove_file(&operation.source)
            .map_err(|error| format!("cannot delete \"{}\": {error}", operation.source_display)),
    }
}

pub(super) fn commit(plan: &[ResolvedPatch], cancel: &Cancel) -> Result<String, ToolError> {
    commit_operations(plan, cancel, execute_one)
}

pub(super) fn commit_operations(
    plan: &[ResolvedPatch],
    cancel: &Cancel,
    mut execute: impl FnMut(&ResolvedPatch) -> Result<(), String>,
) -> Result<String, ToolError> {
    let mut completed: Vec<&ResolvedPatch> = Vec::new();
    for (index, operation) in plan.iter().enumerate() {
        if cancel.is_cancelled() {
            return if completed.is_empty() {
                Err(ToolError::Cancelled)
            } else {
                Err(ToolError::Failed(cancelled_output(plan, &completed, index)))
            };
        }
        match execute(operation) {
            Ok(()) => completed.push(operation),
            Err(message) => {
                return Err(ToolError::Failed(stopped_output(
                    plan, &completed, index, &message,
                )));
            }
        }
    }
    Ok(success_output(plan))
}

fn success_output(plan: &[ResolvedPatch]) -> String {
    if plan.is_empty() {
        return "Success. No operations in patch.".into();
    }
    let mut lines = vec!["Success. Updated the following files:".to_string()];
    push_completed(&mut lines, &plan.iter().collect::<Vec<_>>());
    lines.join("\n")
}

fn push_completed(lines: &mut Vec<String>, completed: &[&ResolvedPatch]) {
    for group in [Kind::Add, Kind::Update, Kind::Delete] {
        for operation in completed {
            let current = if operation.kind == Kind::Move {
                Kind::Update
            } else {
                operation.kind
            };
            if current == group {
                lines.push(operation.completed_line());
            }
        }
    }
}

fn stopped_output(
    plan: &[ResolvedPatch],
    completed: &[&ResolvedPatch],
    failed_index: usize,
    message: &str,
) -> String {
    let mut lines = vec![format!(
        "Patch stopped after {} of {} operations.",
        completed.len(),
        plan.len()
    )];
    if !completed.is_empty() {
        lines.push("Completed:".into());
        push_completed(&mut lines, completed);
    }
    lines.push("Failed:".into());
    let failed = &plan[failed_index];
    lines.push(format!(
        "- operation {} ({}): {message}",
        failed.index + 1,
        failed.describe()
    ));
    push_skipped(&mut lines, &plan[failed_index + 1..]);
    lines.join("\n")
}

fn push_skipped(lines: &mut Vec<String>, skipped: &[ResolvedPatch]) {
    if skipped.is_empty() {
        return;
    }
    lines.push("Not executed:".into());
    for operation in skipped {
        lines.push(format!(
            "- operation {} ({})",
            operation.index + 1,
            operation.describe_short()
        ));
    }
}

fn cancelled_output(
    plan: &[ResolvedPatch],
    completed: &[&ResolvedPatch],
    next_index: usize,
) -> String {
    let mut lines = vec![format!(
        "Patch cancelled after {} of {} operations.",
        completed.len(),
        plan.len()
    )];
    if !completed.is_empty() {
        lines.push("Completed:".into());
        push_completed(&mut lines, completed);
    }
    push_skipped(&mut lines, &plan[next_index..]);
    lines.join("\n")
}
