//! Ignored real-model probe for the step 10 benefit pilot.
//!
//! One invocation runs one task in a disposable copy. The `code` arm adds
//! the four code-index tools and the context arms add only `code_context` to `base`.
//! The probe reuses the production registry, schemas, guidance, system
//! prompt, and tool bodies. It does not use the Tauri run loop, so compaction,
//! provider retries, approval cards, and transcript persistence are out of
//! scope; the comparison is about tool selection and navigation only.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::AppConfig;
use crate::code_index::{CodeIndexConfig, CodeIndexManager};
use crate::tools::{ApprovalDecision, ApprovalRequest, Approver, ToolRegistry, Workspace};

struct Allow;

impl Approver for Allow {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async { ApprovalDecision::Allow })
    }
}

#[tokio::test]
#[ignore = "runs a real model against a disposable workspace; set OAKLET_BENCH to a spec file"]
async fn benefit_pilot() {
    let spec_path = std::env::var("OAKLET_BENCH").expect("OAKLET_BENCH");
    let spec: Value = serde_json::from_str(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
    let result = run(&spec).await;
    let out = Path::new(spec["out"].as_str().unwrap());
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    match result {
        Ok(mut value) => {
            value["group"] = spec["group"].clone();
            value["task"] = spec["task"].clone();
            std::fs::write(out, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        }
        Err(error) => {
            let value = json!({
                "group": spec["group"],
                "task": spec["task"],
                "error": error,
            });
            std::fs::write(out, serde_json::to_string_pretty(&value).unwrap()).unwrap();
            panic!("bench run failed: {}", value["error"]);
        }
    }
}

async fn run(spec: &Value) -> Result<Value, String> {
    let workspace = spec["workspace"].as_str().ok_or("workspace")?;
    let group = spec["group"].as_str().ok_or("group")?;
    let task = spec["prompt"].as_str().ok_or("prompt")?;
    let max_steps = spec["maxSteps"].as_u64().unwrap_or(40) as usize;

    let api_key = std::env::var("OAKLET_BENCH_API_KEY")
        .ok()
        .or_else(|| spec["apiKey"].as_str().map(str::to_string))
        .ok_or("apiKey")?;
    let defaults = AppConfig::default();
    let config = AppConfig {
        workspace: workspace.to_string(),
        base_url: spec["baseUrl"].as_str().ok_or("baseUrl")?.to_string(),
        api_key,
        model: spec["model"].as_str().ok_or("model")?.to_string(),
        context_window: spec["contextWindow"]
            .as_u64()
            .map(|window| window as usize)
            .unwrap_or(defaults.context_window),
        ..defaults
    };

    let mut registry = ToolRegistry::new();
    let mut manager = None;
    let context_arm = matches!(group, "context" | "context_nudge");
    if context_arm || matches!(group, "code" | "code_nudge" | "code_only") {
        let index = Arc::new(CodeIndexManager::new());
        index
            .open_workspace(Path::new(workspace), CodeIndexConfig::default())
            .map_err(|error| format!("open_workspace: {error}"))?;
        if context_arm {
            registry.push(Box::new(crate::tools::code_context::CodeContext(
                index.clone(),
            )));
        } else {
            crate::tools::code::register(&mut registry, index.clone());
        }
        manager = Some(index);
    }

    // Both groups run without a shell so a probe cannot start a workspace
    // build; navigation and edits are the only variables.
    registry.retain(|name| {
        if name == "run_command" {
            return false;
        }
        if group == "code_only" {
            return name.starts_with("code_")
                || matches!(
                    name,
                    "read_file"
                        | "read_image"
                        | "list_dir"
                        | "glob"
                        | "write_file"
                        | "edit_file"
                        | "apply_patch"
                );
        }
        true
    });

    let (mut ctx, root, _owner) = crate::tools::test_support::context("agent-bench");
    ctx.workspace = Workspace::new(workspace).map_err(|error| error.to_string())?;
    ctx.spill_root = root.join("spill");

    let ready_ms = if manager.is_some() {
        let mut args = spec["readyPosition"].clone();
        let required_file = spec["readyReferenceFile"].as_str();
        let tool = if context_arm {
            "code_context"
        } else if let Some(file) = required_file {
            args["reference_file"] = json!(file);
            args["limit"] = json!(1);
            "code_references"
        } else if args["query"].is_string() {
            "code_symbols"
        } else {
            "code_definition"
        };
        let started = Instant::now();
        let deadline = started + Duration::from_secs(240);
        loop {
            let outcome = registry
                .execute(&ctx, &Allow, tool, "ready", args.clone())
                .await;
            let ready = match tool {
                "code_context" => serde_json::from_str::<Value>(&outcome.output)
                    .is_ok_and(|value| value["semantic"] == "answered"),
                "code_references" => required_file.is_some_and(|file| {
                    serde_json::from_str::<Value>(&outcome.output)
                        .is_ok_and(|value| value["entries"][0]["path"].as_str() == Some(file))
                }),
                _ => true,
            };
            if outcome.ok && ready {
                break started.elapsed().as_millis() as u64;
            }
            if Instant::now() >= deadline {
                if let Some(index) = &manager {
                    index.shutdown().await;
                }
                return Err(format!(
                    "index not ready for {tool} {args}: {}",
                    outcome.output
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    } else {
        0
    };

    let mut system = super::messages::system_prompt(&config, &registry, &[], None);
    if let Some(suffix) = spec["systemSuffix"].as_str() {
        system.push_str("\n\n");
        system.push_str(suffix);
    }
    let mut messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": task}),
    ];
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

    let started = Instant::now();
    let mut steps = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut final_text = String::new();
    let mut error = None;

    for step in 0..max_steps {
        let body = json!({
            "model": config.model,
            "messages": messages,
            "tools": registry.schemas(),
            "temperature": 0,
        });
        let response = client
            .post(&url)
            .bearer_auth(&config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("request {step}: {error}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("body {step}: {error}"))?;
        if !status.is_success() {
            error = Some(format!("HTTP {status}: {text}"));
            break;
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("decode {step}: {e}: {}", &text[..text.len().min(300)]))?;
        if let Some(usage) = value.get("usage") {
            prompt_tokens += usage["prompt_tokens"].as_u64().unwrap_or(0);
            completion_tokens += usage["completion_tokens"].as_u64().unwrap_or(0);
        }
        let Some(message) = value["choices"].get(0).map(|choice| &choice["message"]) else {
            error = Some(format!("no choices: {text}"));
            break;
        };
        let content = message["content"].as_str().unwrap_or_default().to_string();
        let calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        messages.push(json!({
            "role": "assistant",
            "content": content,
            "tool_calls": calls,
        }));
        if calls.is_empty() {
            final_text = content;
            break;
        }
        for call in calls {
            let id = call["id"].as_str().unwrap_or_default().to_string();
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let raw = call["function"]["arguments"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let args: Result<Value, String> = if raw.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str(&raw).map_err(|e| format!("Invalid tool arguments: {e}"))
            };
            let call_started = Instant::now();
            let output = match &args {
                Ok(args) => {
                    registry
                        .execute(&ctx, &Allow, &name, &id, args.clone())
                        .await
                }
                Err(message) => crate::tools::ToolOutcome::failure(message.clone()),
            };
            let mut trace = json!({
                "step": step,
                "tool": name,
                "args": raw,
                "ok": output.ok,
                "ms": call_started.elapsed().as_millis() as u64,
                "outputChars": output.output.chars().count(),
                "outputHead": output.output.chars().take(600).collect::<String>(),
            });
            if name == "code_references"
                && let Ok(output) = serde_json::from_str::<Value>(&output.output)
            {
                trace["referenceOutput"] = output;
            }
            if name == "code_context"
                && let Ok(output) = serde_json::from_str::<Value>(&output.output)
                && let Some(status) = output["semantic"].as_str()
            {
                trace["contextSemantic"] = json!(status);
            }
            steps.push(trace);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": output.output,
            }));
        }
        eprintln!(
            "step {step}: {} tool calls, {} prompt tokens",
            messages.iter().filter(|m| m["role"] == "tool").count(),
            prompt_tokens
        );
    }

    if let Some(index) = &manager {
        index.shutdown().await;
    }
    std::fs::remove_dir_all(&root).ok();

    Ok(json!({
        "readyMs": ready_ms,
        "durationMs": started.elapsed().as_millis() as u64,
        "steps": steps,
        "final": final_text,
        "usage": {
            "prompt": prompt_tokens,
            "completion": completion_tokens,
            "total": prompt_tokens + completion_tokens,
        },
        "error": error,
    }))
}
