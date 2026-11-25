use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use super::{ApprovalRequest, Tool, ToolContext, ToolError, ToolOutput, parse_args};
use crate::chat::worktree::{exit, save_entry};
use crate::store::Store;
use crate::worktrees::{self, WorktreeRequest};

pub(crate) struct WorktreeTool {
    pub store: Store,
    pub app: AppHandle,
    pub exiting: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterArgs {
    name: Option<String>,
    from: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitArgs {}

impl Tool for WorktreeTool {
    fn name(&self) -> &str {
        if self.exiting {
            "exit_worktree"
        } else {
            "enter_worktree"
        }
    }

    fn description(&self) -> &str {
        if self.exiting {
            "Return this conversation to the checkout it entered from, leaving the worktree and branch on disk."
        } else {
            "Move this conversation into a registered worktree in the current checkout's pool. If the name is absent or has no existing worktree, create a new branch and worktree from HEAD. If from is given, always create from that local revision."
        }
    }

    fn parameters(&self) -> Value {
        if self.exiting {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        } else {
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Worktree name inside the checkout's pool. Optional." },
                    "from": { "type": "string", "description": "Local commit, branch or tag for creating a new worktree. Defaults to HEAD." }
                },
                "additionalProperties": false
            })
        }
    }

    fn approval(&self, args: &Value) -> Option<ApprovalRequest> {
        Some(ApprovalRequest {
            tool: self.name().to_owned(),
            prompt: if self.exiting {
                "Exit this worktree?"
            } else {
                "Enter or create a Git worktree?"
            }
            .to_owned(),
            detail: args.to_string(),
            args: args.clone(),
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let _lock = tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                lock = worktrees::MUTATION.lock() => lock,
            };
            let mut checkout = self
                .store
                .worktree(&ctx.session_id)
                .map_err(ToolError::Failed)?;
            let (entry, created) = if self.exiting {
                let _: ExitArgs = parse_args(args)?;
                (
                    exit(&self.store, &ctx.session_id, &mut checkout).map_err(ToolError::Failed)?,
                    false,
                )
            } else {
                let args: EnterArgs = parse_args(args)?;
                let existing = match (&args.name, &args.from) {
                    (Some(name), None) => worktrees::enter(ctx.workspace.root(), name)
                        .await
                        .map_err(ToolError::Failed)?,
                    _ => None,
                };
                let generated = crate::chat::worktree::generated_name(
                    self.app.state::<crate::SharedState>().inner(),
                    &ctx.session_id,
                );
                let (entry, created) = match existing {
                    Some(entry) => (entry, false),
                    None => (
                        worktrees::create(
                            ctx.workspace.root(),
                            WorktreeRequest {
                                name: args.name,
                                from: args.from,
                            },
                            &generated,
                            &ctx.cancel,
                            |_| {},
                        )
                        .await
                        .map_err(ToolError::Failed)?,
                        true,
                    ),
                };
                save_entry(
                    &self.store,
                    &ctx.session_id,
                    &mut checkout,
                    entry.clone(),
                    created,
                )
                .map_err(ToolError::Failed)?;
                (entry, created)
            };
            let _ = self
                .app
                .emit_to(&ctx.window, "worktree://changed", &ctx.session_id);
            Ok(ToolOutput::text(
                json!({ "checkout": entry, "created": created }).to_string(),
            ))
        })
    }
}
