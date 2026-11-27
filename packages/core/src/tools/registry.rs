use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tauri::Manager;

use super::output::bound_output;
use super::{
    Access, ApprovalDecision, Approver, Cancel, FileDiff, ProcessOutcome, Tool, ToolContext,
    ToolError, ToolOutcome, fs, image, jobs, likec4, mcp, outline, patch, plan, plan_diff,
    plan_present, search, shell, skill, structure, web, web_fetch,
};

/// All tools available to the agent, in model-visible order.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    warnings: Vec<String>,
    access: Arc<Access>,
    planning: Option<crate::architecture::PlanningScope>,
    approved_plan: Option<u64>,
}

impl ToolRegistry {
    /// Tools that need host state beyond the workspace: MCP servers (including
    /// LikeC4) and, when a search key is configured, web search.
    pub async fn with_host_tools(
        app: &tauri::AppHandle,
        config: &crate::AppConfig,
        cancel: &Cancel,
    ) -> Self {
        let mut registry = Self::new();
        let workspace = PathBuf::from(&config.workspace);
        let home = app.path().home_dir().ok();
        let catalog = tauri::async_runtime::spawn_blocking(move || {
            skill::SkillCatalog::discover(&workspace, home.as_deref())
        })
        .await
        .unwrap_or_default();
        // Built-in principles are always available, even when the workspace
        // has no skills of its own, so the tool is always registered.
        registry
            .tools
            .push(Box::new(skill::LoadSkill::new(catalog)));

        let runtime_started = std::time::Instant::now();
        let node = crate::architecture::run_node();
        let helper = crate::architecture::find_helper(app);
        crate::log::key(format_args!(
            "[oaklet] runtime discovery {} ms (node {}, helper {})",
            runtime_started.elapsed().as_millis(),
            node.is_some(),
            helper.is_some()
        ));
        registry.tools.push(Box::new(likec4::LikeC4 {
            node: node.clone(),
            helper: helper.clone(),
        }));
        registry
            .tools
            .push(Box::new(plan_diff::ValidateDiff::new(node, helper)));
        let mcp_started = std::time::Instant::now();
        let (tools, warnings) = mcp::discover(
            &config.mcp_servers,
            std::path::Path::new(&config.workspace),
            cancel,
        )
        .await;
        crate::log::key(format_args!(
            "[oaklet] mcp discovery {} ms ({} tools from {} servers)",
            mcp_started.elapsed().as_millis(),
            tools.len(),
            config.mcp_servers.len()
        ));
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
                Box::new(plan::AssessChange),
                Box::new(plan_present::PresentPlan),
                Box::new(fs::ReadFile),
                Box::new(outline::FileOutline),
                Box::new(image::ReadImage),
                Box::new(fs::WriteFile),
                Box::new(fs::EditFile),
                Box::new(patch::ApplyPatch),
                Box::new(fs::ListDir),
                Box::new(search::Glob),
                Box::new(search::Grep),
                Box::new(structure::StructuralSearch),
                Box::new(shell::RunCommand),
                Box::new(jobs::JobList),
                Box::new(jobs::JobOutput),
                Box::new(jobs::JobKill),
            ],
            warnings: Vec::new(),
            access: Access::execute(),
            planning: None,
            approved_plan: None,
        }
    }

    /// Apply the run's capability gate. Call before building the system prompt.
    pub fn set_access(&mut self, access: Arc<Access>) {
        self.access = access;
    }

    pub fn access(&self) -> &Arc<Access> {
        &self.access
    }

    /// Model selected when this run started, used to focus the system prompt.
    pub fn set_planning_scope(&mut self, planning: Option<crate::architecture::PlanningScope>) {
        self.planning = planning;
    }

    pub fn planning_scope(&self) -> Option<&crate::architecture::PlanningScope> {
        self.planning.as_ref()
    }

    pub fn set_approved_plan(&mut self, version: Option<u64>) {
        self.approved_plan = version;
    }

    pub fn approved_plan(&self) -> Option<u64> {
        self.approved_plan
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
                .filter(|tool| self.visible(tool.as_ref()))
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

    pub fn guidance_named(&self) -> Vec<(&str, &str)> {
        let mut seen = std::collections::HashSet::new();
        self.tools
            .iter()
            .filter(|tool| self.visible(tool.as_ref()))
            .filter_map(|tool| {
                let text = tool.guidance();
                (!text.is_empty() && seen.insert(text)).then(|| (tool.name(), text))
            })
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
            .filter(|tool| self.visible(*tool))
            .map(|tool| tool.preview(ctx, args))
            .unwrap_or_default()
    }

    /// Whether a tool may join a concurrent read group. Unknown and undeclared
    /// tools stay exclusive.
    pub fn is_concurrency_safe(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|tool| tool.is_concurrency_safe())
    }

    /// Whether a tool is callable in this run: read-only runs hide writing
    /// tools, and plan tools only appear for planning or confirmed plan runs.
    fn visible(&self, tool: &dyn Tool) -> bool {
        self.access.mode().allows(tool.access()) && (!tool.plan_only() || self.plan_tools_active())
    }

    /// Plan tools stay hidden in normal execution turns. The scope is absent
    /// when the workspace has no model, so check the access gate too.
    fn plan_tools_active(&self) -> bool {
        self.access.plan_tools() || self.planning.is_some()
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
        if tool.plan_only() && !self.plan_tools_active() {
            return ToolOutcome::failure(
                "Architecture planning is not active. Start the message with @arch to plan a change."
                    .to_string(),
            );
        }
        if !self.access.mode().allows(tool.access()) {
            return ToolOutcome::failure(if self.access.is_locked() {
                "This turn is read-only because a plan is waiting for review. \
                 Revise the plan instead of editing."
            } else if self.access.planning_only() {
                "This turn is read-only architecture planning. Revise the plan and \
                 wait for the user to confirm the plan card."
            } else {
                "This turn is read-only until you classify it with assess_change. \
                 Only a direct implementation with no architectural impact opens \
                 write, patch, shell, and background-job tools."
            });
        }

        if let Some(request) = tool.approval(&args) {
            match approver.approve(request).await {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny => {
                    return ToolOutcome::failure_with_outcome(
                        "The user denied this command.",
                        Some(ProcessOutcome::Denied),
                    );
                }
                ApprovalDecision::Cancelled => {
                    return ToolOutcome::failure_with_outcome(
                        "Cancelled.",
                        Some(ProcessOutcome::Cancelled),
                    );
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
            Ok(output) => {
                // A process that captured output but exited nonzero is still a
                // failed command, so the tool row must not report success.
                let ok = !output.outcome.as_ref().is_some_and(ProcessOutcome::failed);
                ToolOutcome {
                    ok,
                    output: bound_output(ctx, name, call_id, output.text),
                    images: output.images,
                    diffs: output.diffs,
                    outcome: output.outcome,
                }
            }
            Err(error) => {
                let outcome = match &error {
                    ToolError::Timeout(_) => Some(ProcessOutcome::TimedOut),
                    ToolError::Cancelled => Some(ProcessOutcome::Cancelled),
                    _ => None,
                };
                ToolOutcome::failure_with_outcome(
                    bound_output(ctx, name, call_id, error.to_string()),
                    outcome,
                )
            }
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
