//! System prompt and model-history JSON construction.
//!
//! The coordinator rebuilds the system prompt for every request but saves only
//! the conversation, so these helpers also strip stale system entries from the
//! history handed back to the frontend.

use serde_json::{Value, json};

use super::ToolCall;
use crate::AppConfig;
use crate::tools::{ToolImage, ToolRegistry};

pub(super) fn strip_system(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect()
}

pub(crate) fn initial_messages(
    config: &AppConfig,
    tools: &ToolRegistry,
    model_sources: &[String],
    instructions: Option<&str>,
    input: Vec<Value>,
) -> Vec<Value> {
    let mut messages = Vec::with_capacity(input.len() + 1);
    messages.push(json!({
        "role": "system",
        "content": system_prompt(config, tools, model_sources, instructions)
    }));
    messages.extend(
        input
            .into_iter()
            .filter(|message| message.get("role").and_then(Value::as_str) != Some("system")),
    );
    messages
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

pub(super) fn system_prompt(
    config: &AppConfig,
    tools: &ToolRegistry,
    model_sources: &[String],
    instructions: Option<&str>,
) -> String {
    let mut rules: Vec<String> = vec![
        "Use the tools to inspect and edit files. Never invent file contents or claim work you did not do.".into(),
        "Paths are relative to the workspace; absolute paths inside it also work. Shell commands start in the workspace root, so never prefix them with cd <workspace>.".into(),
        "In your reply, name files by their full workspace-relative path, such as `crates/acorn-core/src/ontology/view.rs`. Chat file links resolve from the workspace root, so a module-relative shorthand like `ontology/view.rs` will not open.".into(),
    ];
    rules.extend(tools.guidance().into_iter().map(str::to_string));
    rules.extend(
        tools
            .warnings()
            .iter()
            .map(|warning| format!("MCP warning: {warning}")),
    );
    if !model_sources.is_empty() {
        rules.push(format!(
            "This workspace has LikeC4 architecture sources ({}). Judge the architectural impact \
             before you edit: adding or removing elements, changing a component's responsibilities, or \
             changing dependencies means this turn stays read-only and ends with a plan for review. \
             Read the current model first and preserve its element and view IDs, style, and manual \
             descriptions. For a direct implementation with no architectural impact, update the \
             affected model in the same turn so it matches the code, keeping the edit minimal and \
             grounded in the code you changed. Use likec4_mcp to understand existing elements and \
             relationships, and file tools to edit. Before presenting an architectural plan, \
             validate its complete native diff with validate_architecture_diff; only describe it as \
             verified after the tool reports success. Then call present_plan with the objective, \
             code and model changes, ordered implementation steps, verification, risks, and open \
             questions; those fields become the review card. If the tool is unavailable or you \
             cannot use it, the final reply becomes the plan card, so make it a self-contained \
             ordered list of implementation steps. If the code change has no architectural effect, \
             leave the model unchanged and say so; do not create a model when none exists.",
            model_sources.join(", ")
        ));
    }
    if let Some(scope) = tools.planning_scope() {
        rules.push(format!(
            "Architecture planning is scoped to the LikeC4 model root `{}`. Query that model with \
             likec4_mcp before planning, and validate the complete draft with \
             validate_architecture_diff.",
            scope.model_root
        ));
    }
    if let Some(version) = tools.approved_plan() {
        rules.push(format!(
            "The user approved plan version {version} from the plan card. This run may edit from \
             the first step: implement the approved plan and any architecture diff included in the confirmation, and do not \
             re-open planning for that same change. If implementation reveals a new architectural \
             change beyond the approved plan, stop writing and report it using assess_change \
             with intent=revise and architecture_impact=possible or confirmed. List any changes \
             already made, then prepare the remaining work for a new review."
        ));
    }
    rules.push("Keep the final answer short: what changed and how to verify it.".into());
    let mut prompt = format!(
        "You are Oaklet, a coding agent running locally in a desktop app. You help the user \
         inspect and change the project in their workspace.\n\n\
         Workspace: {workspace}\nPlatform: {platform}\n\n\
         Rules:\n- {rules}",
        workspace = config.workspace,
        platform = std::env::consts::OS,
        rules = rules.join("\n- "),
    );
    if let Some(instructions) = instructions {
        prompt.push_str("\n\nWorkspace instructions:\n\n");
        prompt.push_str(instructions);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_the_system_prompt_and_drops_stale_ones() {
        let config = AppConfig::default();
        let tools = ToolRegistry::new();
        let messages = initial_messages(
            &config,
            &tools,
            &[],
            None,
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
        let prompt = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("Commands may need approval"));
        assert!(prompt.contains("never prefix them with cd <workspace>"));
        assert!(prompt.contains("Prefer edit_file"));
        assert!(prompt.contains("Read a file before editing it"));
    }

    #[test]
    fn system_prompt_asks_for_full_workspace_paths_in_replies() {
        let prompt = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(prompt.contains("full workspace-relative path"));
        assert!(prompt.contains("module-relative shorthand"));
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
        let prompt = system_prompt(&AppConfig::default(), &tools, &[], None);

        assert!(prompt.contains("Available skills"));
        assert!(prompt.contains("`review`: Review a change."));
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn system_prompt_includes_workspace_instructions() {
        let config = AppConfig::default();
        let tools = ToolRegistry::new();
        let prompt = system_prompt(
            &config,
            &tools,
            &[],
            Some("Instructions from: AGENTS.md\n\nroot rules"),
        );
        assert!(prompt.contains("Workspace instructions:"));
        assert!(prompt.contains("root rules"));

        let prompt = system_prompt(&config, &tools, &[], None);
        assert!(!prompt.contains("Workspace instructions:"));
    }

    #[test]
    fn system_prompt_requests_architecture_updates_when_models_exist() {
        let sources = vec!["architecture/model.c4".to_string()];
        let prompt = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &sources, None);
        assert!(prompt.contains("architecture/model.c4"));
        assert!(prompt.contains("Judge the architectural impact"));
        assert!(prompt.contains("stays read-only and ends with a plan"));
        assert!(prompt.contains("update the affected model"));
        assert!(prompt.contains("validate_architecture_diff"));

        let without_sources = system_prompt(&AppConfig::default(), &ToolRegistry::new(), &[], None);
        assert!(!without_sources.contains("LikeC4 architecture sources"));
    }

    #[test]
    fn system_prompt_names_the_selected_architecture_model() {
        let mut tools = ToolRegistry::new();
        tools.set_planning_scope(Some(crate::architecture::PlanningScope {
            model_root: "architecture".into(),
            include_ignored: false,
        }));
        let prompt = system_prompt(&AppConfig::default(), &tools, &[], None);
        assert!(prompt.contains("model root `architecture`"));
        assert!(prompt.contains("validate_architecture_diff"));
    }

    #[test]
    fn system_prompt_opens_the_tools_for_an_approved_plan() {
        let mut tools = ToolRegistry::new();
        tools.set_approved_plan(Some(3));
        let prompt = system_prompt(&AppConfig::default(), &tools, &[], None);
        assert!(prompt.contains("approved plan version 3"));
        assert!(prompt.contains("may edit from the first step"));
        assert!(prompt.contains("stop writing and report it"));
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
