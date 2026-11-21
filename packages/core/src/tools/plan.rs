//! Planning gate: classify a turn before any tool can change the workspace.
//!
//! The model calls `assess_change` once at the start of a turn. The backend
//! derives the run's access from that classification; the tool never accepts
//! the decision from the model directly.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ArchitectureImpact, Assessment, Intent, Tool, ToolAccess, ToolContext, ToolError, ToolOutput,
    parse_args,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    intent: Intent,
    architecture_impact: ArchitectureImpact,
}

pub struct AssessChange;

impl Tool for AssessChange {
    fn name(&self) -> &str {
        "assess_change"
    }

    fn description(&self) -> &'static str {
        "Classify this turn before acting. Returns whether edit, patch, shell, and background-job tools are open. Call it once at the start of a turn, and again if implementation reveals an architectural impact."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "enum": ["implement", "evaluate", "revise", "question"],
                    "description": "implement: change the code now. evaluate: assess a possible change and stop at a plan. revise: update a plan already under review. question: answer without changing anything."
                },
                "architecture_impact": {
                    "type": "string",
                    "enum": ["none", "possible", "confirmed"],
                    "description": "none: confined to existing components. possible or confirmed: adds or removes architecture elements, changes a component's responsibilities, or changes dependencies."
                }
            },
            "required": ["intent", "architecture_impact"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Call assess_change once at the start of an architecture planning turn. It records the intent and impact for the review card, and planning stays read-only either way. Use intent=evaluate for a change under consideration, revise when updating a plan already under review, and question when you only need to answer. Investigate with read_file, list_dir, glob, grep, and likec4_mcp, then validate the draft and present the plan. If the request is ambiguous, use intent=question and ask one short clarifying question. After the user confirms a plan, a new architectural change revealed during implementation must stop writes and open a new review."
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn plan_only(&self) -> bool {
        true
    }

    fn summary(&self, args: &Value) -> String {
        match (
            args.get("intent").and_then(Value::as_str),
            args.get("architecture_impact").and_then(Value::as_str),
        ) {
            (Some(intent), Some(impact)) => format!("{intent} / {impact}"),
            _ => String::new(),
        }
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let arguments: Arguments = parse_args(args)?;
            let assessment = Assessment {
                intent: arguments.intent,
                impact: arguments.architecture_impact,
            };
            let previous = ctx.access.mode();
            let mode = ctx.access.assess(assessment);
            let text = if mode == ToolAccess::Execute {
                "Classification recorded. You may use write, patch, shell, and background-job tools. \
                 If implementation reveals architecture work outside the current authorization, call \
                 assess_change again before the next write."
                    .to_string()
            } else if previous == ToolAccess::Execute && assessment.needs_plan() {
                ctx.record_plan_validation(None);
                "Execution was paused. List the changes already made, then investigate and prepare \
                 a new plan and validated architecture draft for the remaining work. Wait for review."
                    .to_string()
            } else if ctx.access.is_locked() {
                "Classification recorded. This turn stays read-only because a plan is already \
                 waiting for review. Revise the plan, or explain what the user should confirm; \
                 do not edit."
                    .to_string()
            } else if assessment.intent == Intent::Question {
                "Classification recorded. This turn stays read-only: answer the user's question \
                 without editing. If they want a change, ask them to confirm a plan first."
                    .to_string()
            } else {
                "Classification recorded. This turn stays read-only: investigate with read_file, \
                 list_dir, glob, grep, and likec4_mcp, then reply with an ordered implementation \
                 plan naming the affected files and views. Do not claim any change was made; \
                 wait for the user to review."
                    .to_string()
            };
            Ok(ToolOutput::text(text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Access;
    use crate::tools::test_support::context;

    fn assess(access: &Access, intent: Intent, impact: ArchitectureImpact) -> ToolAccess {
        access.assess(Assessment { intent, impact })
    }

    #[test]
    fn only_an_approved_implementation_opens_execution() {
        let access = Access::approved();
        assert_eq!(
            assess(&access, Intent::Implement, ArchitectureImpact::None),
            ToolAccess::Execute
        );

        for impact in [ArchitectureImpact::Possible, ArchitectureImpact::Confirmed] {
            let access = Access::approved();
            assert_eq!(
                assess(&access, Intent::Implement, impact),
                ToolAccess::ReadOnly
            );
        }
    }

    #[test]
    fn planning_access_never_opens_execution() {
        let access = Access::planning();
        assert_eq!(
            assess(&access, Intent::Implement, ArchitectureImpact::None),
            ToolAccess::ReadOnly
        );
        // An @arch turn still ends at the review card.
        assert!(access.assessment().is_some_and(Assessment::needs_plan));
        assert_eq!(
            assess(&access, Intent::Question, ArchitectureImpact::None),
            ToolAccess::ReadOnly
        );
        assert!(!access.assessment().is_some_and(Assessment::needs_plan));
    }

    #[test]
    fn a_locked_run_never_opens_execution() {
        let access = Access::locked_read_only();
        assert_eq!(
            assess(&access, Intent::Implement, ArchitectureImpact::None),
            ToolAccess::ReadOnly
        );
        assert!(access.is_locked());
    }

    #[test]
    fn implementation_can_fall_back_to_planning() {
        let access = Access::approved();
        assert_eq!(
            assess(&access, Intent::Implement, ArchitectureImpact::None),
            ToolAccess::Execute
        );
        assert_eq!(
            assess(&access, Intent::Implement, ArchitectureImpact::Confirmed),
            ToolAccess::ReadOnly
        );
        assert!(access.assessment().is_some_and(Assessment::needs_plan));
    }

    #[tokio::test]
    async fn reports_the_mode_and_rejects_unknown_fields() {
        let (ctx, root, _handle) = context("assess");
        let mut ctx = ctx;
        ctx.access = Access::approved();
        let output = AssessChange
            .execute(
                &ctx,
                json!({"intent": "implement", "architecture_impact": "none"}),
            )
            .await
            .unwrap();
        assert!(output.contains("write, patch, shell"));
        assert_eq!(ctx.access.mode(), ToolAccess::Execute);
        std::fs::remove_dir_all(root).ok();

        let (ctx, root, _handle) = context("assess-args");
        let mut ctx = ctx;
        ctx.access = Access::approved();
        assert!(matches!(
            AssessChange
                .execute(
                    &ctx,
                    json!({"intent": "implement", "architecture_impact": "none", "x": 1})
                )
                .await,
            Err(ToolError::InvalidArguments(_))
        ));
        std::fs::remove_dir_all(root).ok();
    }
}
