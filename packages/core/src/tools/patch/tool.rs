//! The `apply_patch` tool adapter: arguments, previews, and the entry point.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::commit::commit;
use super::parse::{PatchOperation, parse_patch};
use super::plan::{ResolvedPatch, plan, preflight};
use crate::tools::diagnostics::append_warnings;
use crate::tools::diff::{FileDiff, compute_diffs};
use crate::tools::{Tool, ToolContext, ToolError, ToolOutput, parse_args};

pub struct ApplyPatch;

/// Call-time preview of the requested operations, without touching disk.
fn preview_diffs(operations: &[PatchOperation]) -> Vec<FileDiff> {
    let mut diffs = Vec::new();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, content, .. } => diffs.push(FileDiff {
                path: path.clone(),
                old_text: None,
                new_text: content.clone(),
            }),
            PatchOperation::Delete { .. } => {}
            PatchOperation::Update { path, hunks, .. } => {
                let old = hunks
                    .iter()
                    .flat_map(|hunk| hunk.old.iter().cloned())
                    .collect::<Vec<_>>();
                let new = hunks
                    .iter()
                    .flat_map(|hunk| hunk.new.iter().cloned())
                    .collect::<Vec<_>>();
                if !old.is_empty() || !new.is_empty() {
                    diffs.push(FileDiff {
                        path: path.clone(),
                        old_text: (!old.is_empty()).then(|| old.join("\n")),
                        new_text: new.join("\n"),
                    });
                }
            }
        }
    }
    diffs
}

/// Applied diffs for every file the completed patch touched.
fn applied_diffs(plan: &[ResolvedPatch], before: &[Option<String>]) -> Vec<FileDiff> {
    let mut diffs = Vec::new();
    for (operation, before) in plan.iter().zip(before) {
        match &operation.operation {
            PatchOperation::Add { content, .. } => {
                let after =
                    std::fs::read_to_string(&operation.source).unwrap_or_else(|_| content.clone());
                diffs.extend(compute_diffs(&operation.source_display, None, &after));
            }
            PatchOperation::Delete { .. } => {
                if let Some(before) = before {
                    diffs.extend(compute_diffs(&operation.source_display, Some(before), ""));
                }
            }
            PatchOperation::Update { move_to: None, .. } => {
                if let Some(before) = before
                    && let Ok(after) = std::fs::read_to_string(&operation.source)
                {
                    diffs.extend(compute_diffs(
                        &operation.source_display,
                        Some(before),
                        &after,
                    ));
                }
            }
            PatchOperation::Update {
                move_to: Some(_), ..
            } => {
                if let Some(before) = before {
                    if let Some((destination, display)) = &operation.destination
                        && let Ok(after) = std::fs::read_to_string(destination)
                    {
                        diffs.extend(compute_diffs(display, Some(before), &after));
                    }
                    diffs.extend(compute_diffs(&operation.source_display, Some(before), ""));
                }
            }
        }
    }
    diffs
}

impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Create, update, delete, or move UTF-8 text files with one patch; prefer it over multiple edit_file calls when a change spans several files or separated regions."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Patch text: start with *** Begin Patch and end with *** End Patch. Use *** Add File: path with + lines; *** Update File: path, optionally followed by *** Move to: path, with @@ or @@ context and space-prefixed context, + added, or - removed lines; *** End of File anchors the last hunk to the file end. Use *** Delete File: path to delete. Paths are relative to the workspace."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use one apply_patch for edits already determined for the same independently verifiable change, including multiple hunks in a file or edits across related files. Give each hunk enough context to match one place in the current file. The whole patch is checked before writing. Operations run in order and stop at the first failure. Re-read affected files before retrying when needed because completed operations are kept."
    }

    fn summary(&self, args: &Value) -> String {
        let Some(input) = args.get("input").and_then(Value::as_str) else {
            return String::new();
        };
        let Ok(operations) = parse_patch(input) else {
            return String::new();
        };
        operations
            .iter()
            .take(3)
            .map(PatchOperation::path)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn preview(&self, _ctx: &ToolContext, args: &Value) -> Vec<FileDiff> {
        let Some(input) = args.get("input").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Ok(operations) = parse_patch(input) else {
            return Vec::new();
        };
        preview_diffs(&operations)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { apply_patch(ctx, args) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    input: String,
}

pub(super) fn apply_patch(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
    let args: ApplyPatchArgs = parse_args(args)?;
    if args.input.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "input must contain a patch.".into(),
        ));
    }
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let operations = parse_patch(&args.input).map_err(|(line, reason)| {
        ToolError::InvalidArguments(format!("Invalid patch at line {line}: {reason}"))
    })?;
    let plan = plan(ctx, operations)?;
    let warnings = preflight(&plan, &ctx.cancel)?;
    let before = plan
        .iter()
        .map(|operation| std::fs::read_to_string(&operation.source).ok())
        .collect::<Vec<_>>();
    let mut output = commit(&plan, &ctx.cancel)?;
    append_warnings(&mut output, warnings);
    Ok(ToolOutput::with_diffs(
        output,
        applied_diffs(&plan, &before),
    ))
}
