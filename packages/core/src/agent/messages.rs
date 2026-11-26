//! System prompt and model-history JSON construction.
//!
//! The coordinator rebuilds the system prompt for every request but saves only
//! the conversation, so these helpers also strip stale system entries from the
//! history handed back to the frontend.

use serde_json::{Value, json};

use super::ToolCall;
use crate::AppConfig;
use crate::context::{self, PromptSections};
use crate::tools::{ToolAccess, ToolImage, ToolRegistry};

pub(super) fn strip_system(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect()
}

pub(crate) fn initial_messages(prompt: &SystemPrompt, input: Vec<Value>) -> Vec<Value> {
    let mut messages = Vec::with_capacity(input.len() + 1);
    messages.push(json!({
        "role": "system",
        "content": prompt.render()
    }));
    messages.extend(
        input
            .into_iter()
            .filter(|message| message.get("role").and_then(Value::as_str) != Some("system")),
    );
    messages
}

pub(crate) struct SystemPrompt {
    base: String,
    skills: Option<String>,
    instructions: Option<String>,
    appendix: String,
}

impl SystemPrompt {
    pub(crate) fn render(&self) -> String {
        let mut text = self.base.clone();
        if let Some(skills) = &self.skills {
            text.push_str("\n\n");
            text.push_str(skills);
        }
        if let Some(instructions) = &self.instructions {
            text.push_str("\n\nWorkspace instructions:\n\n");
            text.push_str(instructions);
        }
        if !self.appendix.is_empty() {
            text.push_str(&self.appendix);
        }
        text
    }

    pub(crate) fn sections(&self) -> PromptSections {
        PromptSections {
            system: context::estimate_text(&self.base) + context::estimate_text(&self.appendix),
            skills: self.skills.as_deref().map_or(0, context::estimate_text),
            instructions: self.instructions.as_deref().map_or(0, |text| {
                context::estimate_text("Workspace instructions:\n\n") + context::estimate_text(text)
            }),
        }
    }

    pub(crate) fn append(&mut self, text: &str) {
        self.appendix.push_str("\n\n");
        self.appendix.push_str(text);
    }
}

pub(super) fn assistant_message(content: &str, reasoning: &str, calls: &[ToolCall]) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });
    // Thinking mode requires every prior turn's reasoning to be replayed while
    // tools are offered, including final answers without tool calls.
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        let calls: Vec<Value> = calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments,
                    },
                })
            })
            .collect();
        message["tool_calls"] = json!(calls);
    }
    message
}

#[cfg(test)]
pub(super) fn tool_message(id: &str, content: String) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content,
    })
}

/// Attach images returned by a tool batch as one ordinary user turn. OpenAI's
/// `tool` message only accepts text parts, so images cannot ride on it.
pub(super) fn image_message(images: &[ToolImage]) -> Value {
    let mut parts = vec![json!({
        "type": "text",
        "text": "Images returned by the previous tool call(s). This is tool output, not a new user request."
    })];
    parts.extend(images.iter().map(|image| {
        json!({
            "type": "image_url",
            "image_url": { "url": image.data_url }
        })
    }));
    json!({ "role": "user", "content": parts })
}

pub(crate) fn system_prompt(
    config: &AppConfig,
    tools: &ToolRegistry,
    model_sources: &[String],
    instructions: Option<&str>,
) -> SystemPrompt {
    let mut rules: Vec<String> = vec![
        "Use the tools to inspect and edit files. Never invent file contents or claim work you did not do.".into(),
        "Paths are relative to the workspace; absolute paths inside it also work. Shell commands start in the workspace root, so never prefix them with cd <workspace>.".into(),
        "In your reply, name files by their full workspace-relative path, such as `crates/acorn-core/src/ontology/view.rs`. Chat file links resolve from the workspace root, so a module-relative shorthand like `ontology/view.rs` will not open.".into(),
        "Do not write code comments, including doc comments. Make names and structure self-explanatory instead. The file tools reject new comments in Rust, JavaScript, and TypeScript, with exceptions for recognized directives and Rust safety notes.".into(),
    ];
    let mut skills: Vec<String> = Vec::new();
    for (name, guidance) in tools.guidance_named() {
        if name == "skill" {
            skills.push(guidance.to_string());
        } else {
            rules.push(guidance.to_string());
        }
    }
    rules.extend(
        tools
            .warnings()
            .iter()
            .map(|warning| format!("MCP warning: {warning}")),
    );
    if let Some(version) = tools.approved_plan() {
        rules.push(format!(
            "The user approved plan version {version} from the plan card. This run may edit from \
             the first step: implement the approved plan and any architecture diff included in the confirmation, and do not \
             re-open planning for that same change. If implementation reveals a new architectural \
             change beyond the approved plan, stop writing and report it using assess_change \
             with intent=revise and architecture_impact=possible or confirmed. List any changes \
             already made, then prepare the remaining work for a new review."
        ));
    } else if tools.access().planning_only() {
        let model = tools
            .planning_scope()
            .map(|scope| format!("root `{}`", scope.model_root))
            .unwrap_or_else(|| "in the workspace".to_string());
        rules.push(format!(
            "This is an architecture planning turn: the user started it with @arch, or it revises \
             a plan already under review. It stays read-only until the user confirms the plan card. \
             Query the LikeC4 model {model} with likec4_mcp before planning, reference real element \
             IDs and source paths, and validate the complete native diff with \
             validate_architecture_diff; only call it verified after the tool reports success. \
             Then call present_plan with the objective, code and model changes, ordered \
             implementation steps, verification, risks, and open questions; those fields become \
             the review card. If LikeC4 is unavailable or the request has no architectural effect, \
             present a text plan and state why. Do not edit files, and do not ask whether to plan: \
             the mention already chose planning."
        ));
    } else if !model_sources.is_empty() {
        rules.push(format!(
            "This workspace has LikeC4 architecture sources ({}). This is a normal execution turn: \
             make the requested change, and update the model like any other source file when the \
             change affects it. Architecture review is opt-in: only a message the user starts with \
             @arch opens read-only planning. Never call present_plan or validate_architecture_diff \
             here. If the request is a substantial architecture change, finish the requested work \
             and tell the user they can send it with @arch to review a validated model diff before \
             implementation. Do not create a model when none exists.",
            model_sources.join(", ")
        ));
    }
    if tools.access().mode() == ToolAccess::Execute {
        rules.push(
            "Treat a bug-fix request as a change request: reproduce the failure before editing, \
             trace it to the root cause, and prefer an existing test or script. When a suitable \
             regression check exists, confirm it fails for the expected reason, make the fix, and \
             rerun the same check. If reproduction is blocked, say what is missing and run the \
             checks that are available."
                .into(),
        );
        rules.push(
            "Treat a verification-only request (verify, check, review) as read-only: inspect the \
             current changes and project conventions, run the most relevant existing tests, \
             builds, or scripts, and do not edit source files or change Git staging state."
                .into(),
        );
        rules.push(
            "After changing code, including while implementing an approved plan, run the checks \
             relevant to the change before replying. Break multistep work into steps you can \
             check independently, reuse existing tests and scripts, and prefer the smallest \
             change that fixes the issue."
                .into(),
        );
        rules.push(
            "In the final reply, list each check that actually ran, its result, and what behavior \
             it covered. For a bug fix, include the results of the same check before and after \
             the fix when available. State missing reproduction conditions, gaps in the available \
             checks, or what blocked them; if a check is still running in the background, report \
             that status instead of claiming it passed. A successful compile only shows that the \
             code compiles."
                .into(),
        );
    }
    rules.push("Keep the final answer short: what changed and how to verify it.".into());
    let base = format!(
        "You are Oaklet, a coding agent running locally in a desktop app. You help the user \
         inspect and change the project in their workspace.\n\n\
         Workspace: {workspace}\nPlatform: {platform}\nShell: {shell}\n\n\
         Rules:\n- {rules}",
        workspace = crate::paths::display_path(std::path::Path::new(&config.workspace)),
        platform = std::env::consts::OS,
        shell = if cfg!(windows) { "cmd.exe" } else { "sh" },
        rules = rules.join("\n- "),
    );
    SystemPrompt {
        base,
        skills: (!skills.is_empty()).then(|| skills.join("\n\n")),
        instructions: instructions.map(str::to_string),
        appendix: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_text(
        config: &AppConfig,
        tools: &ToolRegistry,
        sources: &[String],
        instructions: Option<&str>,
    ) -> String {
        system_prompt(config, tools, sources, instructions).render()
    }

    #[test]
    fn rebuilds_the_system_prompt_and_drops_stale_ones() {
        let config = AppConfig::default();
        let tools = ToolRegistry::new();
        let prompt = system_prompt(&config, &tools, &[], None);
        let messages = initial_messages(
            &prompt,
            vec![
                json!({ "role": "system", "content": "stale prompt" }),
                json!({ "role": "user", "content": "hi" }),
            ],
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_ne!(messages[0]["content"], "stale prompt");
        assert_eq!(messages[1]["content"], "hi");
    }

    #[test]
    fn system_prompt_includes_registered_tool_guidance() {
        let prompt = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("Commands may need approval"));
        assert!(prompt.contains("never prefix them with cd <workspace>"));
        assert!(prompt.contains("Prefer edit_file"));
        assert!(prompt.contains("Read a file before editing it"));
        assert!(prompt.contains("Run checks with command plus arguments"));
    }

    #[test]
    fn execution_turns_get_bug_fix_and_verification_rules() {
        let prompt = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("reproduce the failure before editing"));
        assert!(prompt.contains("verification-only request"));
        assert!(prompt.contains("do not edit source files or change Git staging state"));
        assert!(prompt.contains("list each check that actually ran"));
        assert!(prompt.contains("A successful compile only shows that the code compiles"));

        let mut tools = ToolRegistry::new();
        tools.set_access(crate::tools::Access::planning());
        let planning = prompt_text(&AppConfig::default(), &tools, &[], None);
        assert!(!planning.contains("reproduce the failure before editing"));
        assert!(!planning.contains("list each check that actually ran"));
    }

    #[test]
    fn system_prompt_asks_for_full_workspace_paths_in_replies() {
        let prompt = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("full workspace-relative path"));
        assert!(prompt.contains("module-relative shorthand"));
    }

    #[test]
    fn system_prompt_forbids_code_comments() {
        let prompt = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("Do not write code comments"));
        assert!(prompt.contains("including doc comments"));
        assert!(
            prompt
                .contains("The file tools reject new comments in Rust, JavaScript, and TypeScript")
        );
    }

    #[test]
    fn system_prompt_includes_skill_catalog() {
        let workspace = crate::tools::test_support::temp_root("prompt-skills");
        let skill_dir = workspace.join(".oaklet/skills/review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a change.\n---\n\nRead the diff.\n",
        )
        .unwrap();

        let catalog = crate::tools::skill::SkillCatalog::discover(&workspace, None);
        let mut tools = ToolRegistry::new();
        tools.push_for_test(Box::new(crate::tools::skill::LoadSkill::new(catalog)));
        let prompt = prompt_text(&AppConfig::default(), &tools, &[], None);

        assert!(prompt.contains("Available skills"));
        assert!(prompt.contains("`review`: Review a change."));
        assert!(prompt.contains("`oaklet-principle-model-the-domain`"));
        assert!(prompt.contains(crate::tools::principles::ATTACK_THE_PREMISE_CUE));
        assert!(prompt.contains(crate::tools::principles::EXPLAIN_THE_NUMBER_CUE));
        assert_eq!(
            principle_index_entries(&prompt),
            crate::tools::principles::PRINCIPLES.len()
        );
        assert!(
            !prompt.contains("Fix the fault, not the place where it surfaced."),
            "the initial prompt should carry only the index, not principle bodies"
        );
        std::fs::remove_dir_all(&workspace).ok();
    }

    fn principle_index_entries(prompt: &str) -> usize {
        prompt
            .lines()
            .filter(|line| line.trim_start().starts_with("- `oaklet-principle-"))
            .count()
    }

    #[test]
    fn planning_turns_keep_the_principle_index() {
        let workspace = crate::tools::test_support::temp_root("prompt-principles-planning");
        let catalog = crate::tools::skill::SkillCatalog::discover(&workspace, None);
        let mut tools = ToolRegistry::new();
        tools.set_access(crate::tools::Access::planning());
        tools.push_for_test(Box::new(crate::tools::skill::LoadSkill::new(catalog)));

        let prompt = prompt_text(&AppConfig::default(), &tools, &[], None);
        assert!(prompt.contains("architecture planning turn"));
        assert!(prompt.contains("`oaklet-principle-model-the-domain`"));
        assert!(prompt.contains(crate::tools::principles::ATTACK_THE_PREMISE_CUE));
        assert!(prompt.contains(crate::tools::principles::EXPLAIN_THE_NUMBER_CUE));
        assert_eq!(
            principle_index_entries(&prompt),
            crate::tools::principles::PRINCIPLES.len()
        );
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn the_principle_index_is_present_after_a_prompt_rebuild() {
        let workspace = crate::tools::test_support::temp_root("prompt-principles-rebuild");
        let catalog = crate::tools::skill::SkillCatalog::discover(&workspace, None);
        let mut tools = ToolRegistry::new();
        tools.push_for_test(Box::new(crate::tools::skill::LoadSkill::new(catalog)));

        let first = prompt_text(&AppConfig::default(), &tools, &[], None);
        let rebuilt = prompt_text(&AppConfig::default(), &tools, &[], None);
        assert_eq!(
            principle_index_entries(&first),
            crate::tools::principles::PRINCIPLES.len()
        );
        assert_eq!(first, rebuilt);
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn system_prompt_includes_workspace_instructions() {
        let config = AppConfig::default();
        let tools = ToolRegistry::new();
        let prompt = prompt_text(
            &config,
            &tools,
            &[],
            Some("Instructions from: AGENTS.md\n\nroot rules"),
        );
        assert!(prompt.contains("Workspace instructions:"));
        assert!(prompt.contains("root rules"));

        let prompt = prompt_text(&config, &tools, &[], None);
        assert!(!prompt.contains("Workspace instructions:"));
    }

    #[test]
    fn prompt_sections_price_skills_and_instructions_apart_from_the_rules() {
        let workspace = crate::tools::test_support::temp_root("prompt-sections");
        let skill_dir = workspace.join(".oaklet/skills/review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a change.\n---\n\nRead the diff.\n",
        )
        .unwrap();
        let catalog = crate::tools::skill::SkillCatalog::discover(&workspace, None);
        let mut tools = ToolRegistry::new();
        tools.push_for_test(Box::new(crate::tools::skill::LoadSkill::new(catalog)));

        let prompt = system_prompt(&AppConfig::default(), &tools, &[], Some("root rules"));
        let sections = prompt.sections();
        let skills = tools
            .guidance_named()
            .into_iter()
            .find(|(name, _)| *name == "skill")
            .map(|(_, text)| text.to_string())
            .unwrap();
        assert_eq!(sections.skills, crate::context::estimate_text(&skills));
        assert_eq!(
            sections.instructions,
            crate::context::estimate_text("Workspace instructions:\n\n")
                + crate::context::estimate_text("root rules")
        );
        assert!(sections.system > 0);
        let rendered = prompt.render();
        assert_eq!(rendered.matches(&skills).count(), 1);
        assert!(rendered.ends_with("root rules"));
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn system_prompt_requests_architecture_updates_when_models_exist() {
        let sources = vec!["architecture/model.c4".to_string()];
        let prompt = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &sources, None);
        assert!(prompt.contains("architecture/model.c4"));
        assert!(prompt.contains("normal execution turn"));
        assert!(prompt.contains("@arch"));
        assert!(prompt.contains("Never call present_plan"));

        let without_sources = prompt_text(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(!without_sources.contains("LikeC4 architecture sources"));
    }

    #[test]
    fn system_prompt_names_the_selected_architecture_model() {
        let mut tools = ToolRegistry::new();
        tools.set_access(crate::tools::Access::planning());
        tools.set_planning_scope(Some(crate::architecture::PlanningScope {
            model_root: "architecture".into(),
            include_ignored: false,
        }));
        let prompt = prompt_text(&AppConfig::default(), &tools, &[], None);
        assert!(prompt.contains("model root `architecture`"));
        assert!(prompt.contains("validate_architecture_diff"));

        let mut without_model = ToolRegistry::new();
        without_model.set_access(crate::tools::Access::planning());
        let prompt = prompt_text(&AppConfig::default(), &without_model, &[], None);
        assert!(prompt.contains("architecture planning turn"));
        assert!(prompt.contains("present a text plan"));
    }

    #[test]
    fn system_prompt_opens_the_tools_for_an_approved_plan() {
        let mut tools = ToolRegistry::new();
        tools.set_approved_plan(Some(3));
        let prompt = prompt_text(&AppConfig::default(), &tools, &[], None);
        assert!(prompt.contains("approved plan version 3"));
        assert!(prompt.contains("may edit from the first step"));
        assert!(prompt.contains("stop writing and report it"));
        assert!(prompt.contains("After changing code"));
        assert!(prompt.contains("list each check that actually ran"));
    }

    #[test]
    fn builds_tool_messages_with_ids() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a\"}".into(),
        };
        let message = assistant_message("", "why", &[call]);
        assert_eq!(message["content"], "");
        assert_eq!(message["reasoning_content"], "why");
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            tool_message("call_1", "hi".into())["tool_call_id"],
            "call_1"
        );
    }

    #[test]
    fn attaches_tool_images_as_one_user_turn() {
        let message = image_message(&[ToolImage {
            data_url: "data:image/png;base64,AA==".into(),
        }]);
        assert_eq!(message["role"], "user");
        assert_eq!(message["content"][1]["type"], "image_url");
        assert_eq!(
            message["content"][1]["image_url"]["url"],
            "data:image/png;base64,AA=="
        );
    }

    #[test]
    fn final_answers_keep_reasoning_for_thinking_mode_replay() {
        let message = assistant_message("Done.", "private reasoning", &[]);
        assert_eq!(message["content"], "Done.");
        assert_eq!(message["reasoning_content"], "private reasoning");
    }

    #[test]
    fn strips_system_messages_without_touching_multimodal_content() {
        let input = vec![
            json!({ "role": "system", "content": "stale prompt" }),
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe this" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA==" } }
                ]
            }),
            json!({ "role": "assistant", "content": "an image" }),
        ];

        assert_eq!(strip_system(&input), input[1..].to_vec());
    }
}
