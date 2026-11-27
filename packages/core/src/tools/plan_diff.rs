//! Validate a native LikeC4 `diff` draft against the selected model.
//!
//! The draft is compiled as one extra in-memory source file. Nothing is written
//! to the workspace; a successful result carries the baseline digest that the
//! confirmation step must match before starting implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    PLAN_BASELINE_CHANGED, Tool, ToolAccess, ToolContext, ToolError, ToolOutput, parse_args,
};
use crate::architecture::{self, Diagnostic, MAX_SOURCE_FILE_BYTES, run_helper_watch};
use crate::store::{PlanValidation, PlanValidationStatus};

static NEXT_DRAFT_ID: AtomicU64 = AtomicU64::new(1);

pub struct ValidateDiff {
    pub node: Option<PathBuf>,
    pub helper: Option<PathBuf>,
}

impl ValidateDiff {
    pub fn new(node: Option<PathBuf>, helper: Option<PathBuf>) -> Self {
        Self { node, helper }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    dsl: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffSummary {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    added: ChangeCounts,
    removed: ChangeCounts,
    modified: ModifiedCounts,
    #[serde(default)]
    views: Vec<DiffView>,
    #[serde(default)]
    changes: Option<Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ChangeCounts {
    elements: u64,
    relations: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct ModifiedCounts {
    elements: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct DiffView {
    id: String,
    title: String,
    nodes: ViewCounts,
    edges: ViewCounts,
}

#[derive(Clone, Serialize, Deserialize)]
struct ViewCounts {
    added: u64,
    removed: u64,
    modified: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Validation {
    #[serde(flatten)]
    plan: PlanValidation,
    baseline_changed: bool,
}

impl Tool for ValidateDiff {
    fn name(&self) -> &str {
        "validate_architecture_diff"
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn plan_only(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Validate a complete native LikeC4 `diff` DSL draft against the selected architecture model. The backend adds it as a virtual source file and returns the model root, baseline sourceDigest, diff ID and summary, affected views, diagnostics, and overlap warnings. No workspace file is changed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "dsl": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The complete draft, including one top-level `diff <id> '<title>' { ... }` declaration."
                }
            },
            "required": ["dsl"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Before presenting an architectural plan, validate its complete native diff with validate_architecture_diff. Query the selected model first, reference real element IDs and source paths, and only call a diff verified after `status` is \"verified\". If multiple models exist and none is selected, ask the user to choose one in the Architecture panel. Fix diagnostics and validate again; overlap warnings do not block review. When `status` is \"unavailable\", keep a text plan and state the reason. When `baselineChanged` is true, explain that a new planning turn is needed."
    }

    fn summary(&self, _args: &Value) -> String {
        "validate plan diff".into()
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: Args = parse_args(args)?;
            if args.dsl.trim().is_empty() {
                return Err(ToolError::InvalidArguments(
                    "The architecture diff DSL is empty.".into(),
                ));
            }
            if args.dsl.len() as u64 > MAX_SOURCE_FILE_BYTES {
                return Err(ToolError::Failed(format!(
                    "The diff is {} bytes; a plan draft must be at most {MAX_SOURCE_FILE_BYTES} bytes.",
                    args.dsl.len()
                )));
            }
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let include_ignored = ctx
                .planning
                .as_ref()
                .is_some_and(|scope| scope.include_ignored);

            let model = load_model(ctx).await?;
            if ctx
                .access
                .plan_baseline_changed(model.as_ref().ok().map(|model| model.digest.as_str()))
            {
                let plan = PlanValidation {
                    status: PlanValidationStatus::Invalid,
                    dsl: args.dsl,
                    error: Some(PLAN_BASELINE_CHANGED.into()),
                    ..Default::default()
                };
                return Ok(respond(ctx, plan, true));
            }

            let model = match model {
                Ok(model) => model,
                Err(error) => {
                    let plan = unavailable(
                        ctx.planning
                            .as_ref()
                            .map(|scope| scope.model_root.as_str())
                            .unwrap_or(""),
                        include_ignored,
                        "",
                        args.dsl,
                        error,
                        Vec::new(),
                    );
                    return Ok(respond(ctx, plan, false));
                }
            };

            let Some(node) = self.node.as_ref() else {
                let plan = unavailable(
                    &model.root,
                    include_ignored,
                    &model.digest,
                    args.dsl,
                    "LikeC4 validation requires the bundled Node.js runtime. Reinstall Oaklet, or set OAKLET_NODE.".into(),
                    model.warnings,
                );
                return Ok(respond(ctx, plan, false));
            };
            let Some(helper) = self.helper.as_ref() else {
                let plan = unavailable(
                    &model.root,
                    include_ignored,
                    &model.digest,
                    args.dsl,
                    "The LikeC4 compiler helper is missing. Reinstall Oaklet or set OAKLET_ARCHITECTURE_HELPER.".into(),
                    model.warnings,
                );
                return Ok(respond(ctx, plan, false));
            };

            let draft = draft_path(&model.root, &model.sources);
            let request = json!({
                "version": 1,
                "sources": model.sources,
                "diff": { "path": draft, "text": args.dsl },
            });
            let response = run_helper_watch(node, helper, &request, ctx.cancel.clone()).await;
            let mut response = match response {
                Ok(response) => response,
                Err(error) => {
                    let plan = unavailable(
                        &model.root,
                        include_ignored,
                        &model.digest,
                        args.dsl,
                        error,
                        model.warnings,
                    );
                    return Ok(respond(ctx, plan, false));
                }
            };

            let mut warnings = std::mem::take(&mut response.warnings);
            let model_warnings = model.warnings;
            warnings.extend(model_warnings.iter().cloned().map(|message| Diagnostic {
                message,
                file: None,
                line: None,
                column: None,
            }));
            warnings.truncate(50);

            if !response.ok {
                let plan = unavailable_or_invalid(
                    &model.root,
                    include_ignored,
                    &model.digest,
                    args.dsl,
                    response,
                    warnings,
                );
                return Ok(respond(ctx, plan, false));
            }
            let Some(diff) = response.diff else {
                let plan = unavailable(
                    &model.root,
                    include_ignored,
                    &model.digest,
                    args.dsl,
                    "LikeC4 did not return a diff summary.".into(),
                    model_warnings,
                );
                return Ok(respond(ctx, plan, false));
            };
            let diff: DiffSummary = match serde_json::from_value(diff) {
                Ok(diff) => diff,
                Err(error) => {
                    let plan = unavailable(
                        &model.root,
                        include_ignored,
                        &model.digest,
                        args.dsl,
                        format!("Could not read the LikeC4 diff summary: {error}"),
                        model_warnings,
                    );
                    return Ok(respond(ctx, plan, false));
                }
            };

            let plan = PlanValidation {
                status: PlanValidationStatus::Verified,
                valid: true,
                model_root: model.root,
                include_ignored,
                source_digest: model.digest,
                diff_id: Some(diff.id.clone()),
                dsl: args.dsl,
                diff: serde_json::to_value(diff).ok(),
                diagnostics: Vec::new(),
                warnings,
                overlaps: response.overlaps,
                error: None,
            };
            Ok(respond(ctx, plan, false))
        })
    }
}

pub(super) async fn load_model(
    ctx: &ToolContext,
) -> Result<Result<architecture::PlanningModel, String>, ToolError> {
    let workspace = ctx.workspace.clone();
    let planning = ctx.planning.clone();
    match tokio::task::spawn_blocking(move || {
        architecture::planning_model(&workspace, planning.as_ref())
    })
    .await
    {
        Ok(result) => Ok(result),
        Err(error) => Err(ToolError::Failed(format!(
            "Could not read the architecture model: {error}"
        ))),
    }
}

fn unavailable_or_invalid(
    root: &str,
    include_ignored: bool,
    digest: &str,
    dsl: String,
    response: architecture::HelperResponse,
    warnings: Vec<Diagnostic>,
) -> PlanValidation {
    let error = response
        .error
        .unwrap_or_else(|| "The draft could not be validated.".into());
    let diagnostics = response.diagnostics;
    if response.draft_error {
        PlanValidation {
            status: PlanValidationStatus::Invalid,
            valid: false,
            model_root: root.into(),
            include_ignored,
            source_digest: digest.into(),
            diff_id: None,
            dsl,
            diff: None,
            diagnostics,
            warnings,
            overlaps: response.overlaps,
            error: Some(error),
        }
    } else {
        PlanValidation {
            status: PlanValidationStatus::Unavailable,
            valid: false,
            model_root: root.into(),
            include_ignored,
            source_digest: digest.into(),
            diff_id: None,
            dsl,
            diff: None,
            diagnostics,
            warnings,
            overlaps: response.overlaps,
            error: Some(format!(
                "The selected architecture model could not be compiled: {error}"
            )),
        }
    }
}

fn unavailable(
    root: &str,
    include_ignored: bool,
    digest: &str,
    dsl: String,
    error: String,
    notes: Vec<String>,
) -> PlanValidation {
    PlanValidation {
        status: PlanValidationStatus::Unavailable,
        valid: false,
        model_root: root.into(),
        include_ignored,
        source_digest: digest.into(),
        diff_id: None,
        dsl,
        diff: None,
        diagnostics: Vec::new(),
        warnings: notes
            .into_iter()
            .map(|message| Diagnostic {
                message,
                file: None,
                line: None,
                column: None,
            })
            .collect(),
        overlaps: Vec::new(),
        error: Some(error),
    }
}

fn respond(ctx: &ToolContext, plan: PlanValidation, baseline_changed: bool) -> ToolOutput {
    ctx.record_plan_validation(Some(&plan));
    let text = serde_json::to_string_pretty(&Validation {
        plan,
        baseline_changed,
    })
    .unwrap_or_else(|error| format!("Could not encode the validation result: {error}"));
    ToolOutput::text(text)
}

pub(crate) fn draft_path(root: &str, sources: &BTreeMap<String, String>) -> String {
    loop {
        let id = NEXT_DRAFT_ID.fetch_add(1, Ordering::Relaxed);
        let path = if root == "." {
            format!(".oaklet-plan-{id}.c4")
        } else {
            format!("{root}/.oaklet-plan-{id}.c4")
        };
        if !sources.contains_key(&path) {
            return path;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;
    use crate::architecture::{PlanningScope, find_node};
    use crate::tools::{Access, CancelHandle, Workspace};

    const WORKER_DRAFT: &str = r#"diff worker 'Add background worker' {
  add {
    extend api {
      queue = component 'Queue'
    }
    api.web -> api.queue 'Enqueues'
  }
}
"#;

    fn fixture() -> (PathBuf, PathBuf) {
        let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../architecture-helper")
            .canonicalize()
            .unwrap();
        let workspace = helper.join("test/fixtures/valid");
        (helper, workspace)
    }

    fn tool() -> ValidateDiff {
        let (helper, _) = fixture();
        ValidateDiff::new(
            Some(find_node().expect("Node is required for architecture tests")),
            Some(helper),
        )
    }

    fn planning_context(planning: Option<PlanningScope>) -> (ToolContext, PathBuf, CancelHandle) {
        let (_, root) = fixture();
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (handle, cancel) = CancelHandle::new();
        (
            ToolContext {
                workspace,
                cancel,
                access: Access::execute(),
                planning,
                plan_validation: None,
                plan_details: None,
                jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
                session_id: "plan-diff-test".into(),
                window: "main".into(),
                spill_root: root.join("spill"),
            },
            root,
            handle,
        )
    }

    fn parse(output: ToolOutput) -> Value {
        serde_json::from_str(&output).unwrap()
    }

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn validates_a_diff_against_the_only_model() {
        let (ctx, _root, _handle) = planning_context(None);
        let value = parse(
            tool()
                .execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "verified", "{value}");
        assert_eq!(value["valid"], true);
        assert_eq!(value["modelRoot"], "architecture");
        assert_eq!(value["diffId"], "worker");
        assert_eq!(value["diff"]["added"]["elements"], 1);
        assert_eq!(value["diff"]["added"]["relations"], 1);
        assert_eq!(value["diff"]["views"][0]["id"], "containers");
        assert_eq!(value["dsl"], WORKER_DRAFT);
        assert!(
            value["sourceDigest"]
                .as_str()
                .is_some_and(|d| d.len() == 16)
        );
    }

    #[tokio::test]
    async fn captures_the_latest_validation_for_the_plan_event() {
        let (mut ctx, _root, _handle) = planning_context(None);
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        ctx.plan_validation = Some(slot.clone());
        let value = parse(
            tool()
                .execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "verified");
        let captured = slot.lock().unwrap().clone().expect("validation captured");
        assert_eq!(captured.status, PlanValidationStatus::Verified);
        assert_eq!(captured.diff_id.as_deref(), Some("worker"));
        assert!(!captured.include_ignored);
        assert_eq!(
            captured.source_digest,
            value["sourceDigest"].as_str().unwrap()
        );
    }

    #[tokio::test]
    async fn returns_diagnostics_for_an_invalid_diff() {
        let (ctx, _root, _handle) = planning_context(None);
        let value = parse(
            tool()
                .execute(
                    &ctx,
                    json!({ "dsl": "diff broken 'Broken' {\n add { api.web -> api.missing 'Calls' }\n}\n" }),
                )
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "invalid", "{value}");
        assert_eq!(value["valid"], false);
        assert!(!value["diagnostics"].as_array().unwrap().is_empty());
        assert!(
            value["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("missing")))
        );
    }

    #[tokio::test]
    async fn rejects_more_than_one_top_level_diff() {
        let (ctx, _root, _handle) = planning_context(None);
        let value = parse(
            tool()
                .execute(
                    &ctx,
                    json!({ "dsl": "diff a 'A' { add { extend api { q = component 'Q' } } }\ndiff b 'B' { add { extend api { r = component 'R' } } }\n" }),
                )
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "invalid", "{value}");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| error.contains("exactly one"))
        );
    }

    #[tokio::test]
    async fn reports_multiple_models_and_a_missing_selection() {
        let root = super::super::test_support::temp_root("plan-diff-multiple");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::write(root.join("one/likec4.config.json"), r#"{"name":"one"}"#).unwrap();
        std::fs::write(root.join("one/model.c4"), "model { }").unwrap();
        std::fs::write(root.join("two/likec4.config.json"), r#"{"name":"two"}"#).unwrap();
        std::fs::write(root.join("two/model.c4"), "model { }").unwrap();
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (_handle, cancel) = CancelHandle::new();
        let ctx = ToolContext {
            workspace,
            cancel,
            access: Access::execute(),
            planning: None,
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            session_id: "plan-diff-multiple".into(),
            window: "main".into(),
            spill_root: root.join("spill"),
        };
        let value = parse(
            tool()
                .execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "unavailable");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| error.contains("multiple LikeC4 models"))
        );

        let missing = PlanningScope {
            model_root: "missing".into(),
            include_ignored: false,
        };
        let (_handle, cancel) = CancelHandle::new();
        let ctx = ToolContext {
            workspace: Workspace::new(root.to_str().unwrap()).unwrap(),
            cancel,
            access: Access::execute(),
            planning: Some(missing),
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            session_id: "plan-diff-missing".into(),
            window: "main".into(),
            spill_root: root.join("spill"),
        };
        let value = parse(
            tool()
                .execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "unavailable");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| error.contains("was not found"))
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn reports_unavailable_when_the_helper_is_missing() {
        let (ctx, _root, _handle) = planning_context(None);
        let tool = ValidateDiff::new(None, None);
        let value = parse(
            tool.execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "unavailable");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| error.contains("Node.js"))
        );
    }

    #[tokio::test]
    async fn a_broken_model_is_unavailable_and_keeps_its_diagnostics() {
        let (helper, _) = fixture();
        let workspace =
            Workspace::new(helper.join("test/fixtures/malformed").to_str().unwrap()).unwrap();
        let (_handle, cancel) = CancelHandle::new();
        let ctx = ToolContext {
            workspace,
            cancel,
            access: Access::execute(),
            planning: None,
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            session_id: "plan-diff-broken".into(),
            window: "main".into(),
            spill_root: std::env::temp_dir().join("oaklet-plan-diff-broken"),
        };
        let value = parse(
            tool()
                .execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(value["status"], "unavailable", "{value}");
        assert!(!value["diagnostics"].as_array().unwrap().is_empty());
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| error.contains("could not be compiled"))
        );
    }

    #[tokio::test]
    async fn rejects_a_draft_after_sources_change_between_query_and_validation() {
        let (_, fixture) = fixture();
        let root = super::super::test_support::temp_root("plan-diff-baseline");
        copy_tree(&fixture, &root);
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (_handle, cancel) = CancelHandle::new();
        let mut ctx = ToolContext {
            workspace,
            cancel,
            access: Access::planning(),
            planning: None,
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            session_id: "plan-diff-baseline".into(),
            window: "main".into(),
            spill_root: root.join("spill"),
        };
        let tool = tool();
        let query = super::super::likec4::LikeC4 {
            node: tool.node.clone(),
            helper: tool.helper.clone(),
        };
        query
            .execute(&ctx, json!({ "tool": "list-projects" }))
            .await
            .unwrap();

        let model = root.join("architecture/model.c4");
        let mut text = fs::read_to_string(&model).unwrap();
        text.push_str("\n// changed\n");
        fs::write(&model, text).unwrap();

        // A later query cannot replace the baseline of the first one.
        assert!(matches!(
            query.execute(&ctx, json!({ "tool": "list-projects" })).await,
            Err(ToolError::Failed(message)) if message == PLAN_BASELINE_CHANGED
        ));
        let stale = parse(
            tool.execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(stale["status"], "invalid", "{stale}");
        assert_eq!(stale["baselineChanged"], true);

        ctx.access = Access::planning();
        query
            .execute(&ctx, json!({ "tool": "list-projects" }))
            .await
            .unwrap();
        let updated = parse(
            tool.execute(&ctx, json!({ "dsl": WORKER_DRAFT }))
                .await
                .unwrap(),
        );
        assert_eq!(updated["status"], "verified", "{updated}");
        assert_eq!(updated["baselineChanged"], false);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn rejects_empty_dsl() {
        let (ctx, _root, _handle) = planning_context(None);
        assert!(matches!(
            tool().execute(&ctx, json!({ "dsl": "  " })).await,
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn generated_draft_names_avoid_real_sources() {
        let mut sources = BTreeMap::new();
        let first = draft_path("architecture", &sources);
        sources.insert(first.clone(), String::new());
        let second = draft_path("architecture", &sources);
        assert_ne!(first, second);
        assert!(!sources.contains_key(&second));
    }
}
