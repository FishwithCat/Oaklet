//! Capture the structured plan a planning turn submits for review.
//!
//! The final assistant reply is still the fallback card content, but this tool
//! carries the objective, ordered steps, risks, and open questions the review
//! view needs without scraping Markdown.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolAccess, ToolContext, ToolError, ToolOutput, parse_args};
use crate::store::{PlanDetails, PlanStep};

const MAX_STEPS: usize = 20;
const MAX_ITEMS: usize = 20;
const MAX_TEXT_CHARS: usize = 20_000;
const MAX_FIELD_CHARS: usize = 2_000;
const MAX_TITLE_CHARS: usize = 300;

pub struct PresentPlan;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Args {
    objective: String,
    changes: String,
    steps: Vec<StepArgs>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    questions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StepArgs {
    title: String,
    outcome: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    components: Vec<String>,
    #[serde(default)]
    depends_on: Vec<u64>,
    verification: String,
}

impl Tool for PresentPlan {
    fn name(&self) -> &str {
        "present_plan"
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn description(&self) -> &'static str {
        "Submit the implementation plan for the review card: the objective, the natural-language code and model changes, ordered steps with expected results and verification, risks, and open questions. Call it once after validating the architecture draft, then reply briefly."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Why this change is needed and what it should achieve."
                },
                "changes": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The code and architecture changes in natural language. Mention affected components or files and the overall approach."
                },
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_STEPS,
                    "description": "Implementation steps in execution order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "minLength": 1 },
                            "outcome": {
                                "type": "string",
                                "minLength": 1,
                                "description": "What is true when this step is finished."
                            },
                            "files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Workspace-relative files or paths this step touches."
                            },
                            "components": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Architecture element and relation IDs this step implements, so impact items can point back to it."
                            },
                            "dependsOn": {
                                "type": "array",
                                "items": { "type": "integer", "minimum": 1 },
                                "description": "One-based numbers of earlier steps that must finish first."
                            },
                            "verification": {
                                "type": "string",
                                "minLength": 1,
                                "description": "How to verify this step, such as a test command or a manual check."
                            }
                        },
                        "required": ["title", "outcome", "verification"],
                        "additionalProperties": false
                    }
                },
                "risks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Risks that could change the implementation decision."
                },
                "questions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Decisions the user should settle before implementation starts."
                }
            },
            "required": ["objective", "changes", "steps"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "After investigating and validating the architecture draft, call present_plan once with the complete review content. Keep the objective and changes in natural language, order the steps as they will be implemented, and put the actual verification command or check on each step. List the architecture element or relation IDs in a step's components so the review view can link impact items to that step. Put decision-changing risks and open questions on the plan, not only in the final reply. Then reply briefly that the plan is ready for review."
    }

    fn summary(&self, _args: &Value) -> String {
        "present plan".into()
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: Args = parse_args(args)?;
            let details = normalize(args)?;
            ctx.record_plan_details(&details);
            Ok(ToolOutput::text(
                "Plan review content recorded. Reply briefly that the plan is ready, and wait for the user to confirm or revise it. Do not repeat every step in the final reply.",
            ))
        })
    }
}

fn normalize(args: Args) -> Result<PlanDetails, ToolError> {
    let objective = trimmed(&args.objective, "objective", MAX_TEXT_CHARS)?;
    let changes = trimmed(&args.changes, "changes", MAX_TEXT_CHARS)?;
    if args.steps.is_empty() {
        return Err(ToolError::InvalidArguments(
            "A plan needs at least one implementation step.".into(),
        ));
    }
    if args.steps.len() > MAX_STEPS {
        return Err(ToolError::InvalidArguments(format!(
            "A plan can have at most {MAX_STEPS} implementation steps."
        )));
    }
    let count = args.steps.len();
    let mut steps = Vec::with_capacity(count);
    for (index, step) in args.steps.into_iter().enumerate() {
        let number = index + 1;
        let title = trimmed(&step.title, "step title", MAX_TITLE_CHARS)?;
        let outcome = trimmed(&step.outcome, "step outcome", MAX_FIELD_CHARS)?;
        let verification = trimmed(&step.verification, "step verification", MAX_FIELD_CHARS)?;
        if step.files.len() > MAX_ITEMS || step.components.len() > MAX_ITEMS {
            return Err(ToolError::InvalidArguments(format!(
                "Step {number} can reference at most {MAX_ITEMS} files and {MAX_ITEMS} components."
            )));
        }
        let files = step
            .files
            .into_iter()
            .map(|file| file.trim().to_string())
            .filter(|file| !file.is_empty())
            .collect();
        let components = step
            .components
            .into_iter()
            .map(|component| component.trim().to_string())
            .filter(|component| !component.is_empty())
            .collect();
        let mut depends_on = step.depends_on;
        depends_on.sort_unstable();
        depends_on.dedup();
        if let Some(dependency) = depends_on
            .iter()
            .find(|dependency| **dependency == 0 || **dependency as usize >= number)
        {
            return Err(ToolError::InvalidArguments(format!(
                "Step {number} depends on step {dependency}; dependencies must point to an earlier step."
            )));
        }
        steps.push(PlanStep {
            title,
            outcome,
            files,
            components,
            depends_on,
            verification,
        });
    }
    Ok(PlanDetails {
        objective,
        changes,
        steps,
        risks: strings(args.risks, "risk")?,
        questions: strings(args.questions, "question")?,
    })
}

fn strings(items: Vec<String>, label: &str) -> Result<Vec<String>, ToolError> {
    if items.len() > MAX_ITEMS {
        return Err(ToolError::InvalidArguments(format!(
            "A plan can have at most {MAX_ITEMS} {label}s."
        )));
    }
    items
        .into_iter()
        .map(|item| trimmed(&item, label, MAX_FIELD_CHARS))
        .collect()
}

fn trimmed(value: &str, label: &str, limit: usize) -> Result<String, ToolError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ToolError::InvalidArguments(format!(
            "The plan {label} cannot be empty."
        )));
    }
    if value.chars().count() > limit {
        return Err(ToolError::InvalidArguments(format!(
            "The plan {label} is too long (limit {limit} characters)."
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::context;

    #[test]
    fn normalizes_and_records_the_review_content() {
        let (mut ctx, root, _handle) = context("present-plan");
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        ctx.plan_details = Some(slot.clone());
        let details = normalize(
            serde_json::from_value(json!({
                "objective": " Add a worker ",
                "changes": "Route jobs through a queue.",
                "steps": [
                    {
                        "title": "Add the queue",
                        "outcome": "Jobs can be enqueued.",
                        "files": ["src/queue.rs", " "],
                        "components": ["api.queue"],
                        "verification": "cargo test queue"
                    },
                    {
                        "title": "Start the worker",
                        "outcome": "Queued jobs run.",
                        "dependsOn": [1, 1],
                        "verification": "cargo test worker"
                    }
                ],
                "risks": ["Queue delivery is at least once."],
                "questions": ["Should retries be bounded?"]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(details.objective, "Add a worker");
        assert_eq!(details.steps.len(), 2);
        assert_eq!(details.steps[0].files, vec!["src/queue.rs"]);
        assert_eq!(details.steps[1].depends_on, vec![1]);
        ctx.record_plan_details(&details);
        assert_eq!(
            slot.lock().unwrap().as_ref().unwrap().questions,
            vec!["Should retries be bounded?"]
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_forward_dependencies_and_empty_steps() {
        let base = json!({
            "objective": "Do it",
            "changes": "Change it",
            "steps": [{
                "title": "One",
                "outcome": "Done",
                "dependsOn": [1],
                "verification": "check"
            }]
        });
        let args: Args = serde_json::from_value(base).unwrap();
        let error = normalize(args).unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));

        let empty: Args = serde_json::from_value(json!({
            "objective": "Do it",
            "changes": "Change it",
            "steps": []
        }))
        .unwrap();
        assert!(matches!(
            normalize(empty),
            Err(ToolError::InvalidArguments(_))
        ));
    }
}
