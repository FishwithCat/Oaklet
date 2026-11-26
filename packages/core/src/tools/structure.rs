use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::syntax::{Grammar, Structure};

use super::search::{MAX_GREP_MATCHES, SEARCH_TIMEOUT, compile_glob, matches, scan_text_files};
use super::{
    Cancel, TOOL_OUTPUT_THRESHOLD_CHARS, Tool, ToolAccess, ToolContext, ToolError, ToolOutput,
    Workspace, parse_args,
};

const MAX_FILES: usize = 2_000;
const TRUNCATED: &str = "\n[Search incomplete. Narrow path or include to continue.]";

pub(super) struct StructuralSearch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    query: Structure,
    path: Option<String>,
    include: Option<String>,
}

impl Tool for StructuralSearch {
    fn name(&self) -> &str {
        "structural_search"
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn description(&self) -> &'static str {
        "Find code constructs in saved workspace files: JavaScript or TypeScript invoke(...) calls, Rust #[tauri::command] definitions, or Rust #[test] definitions. Returns paths and line numbers."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "enum": ["invoke_call", "tauri_command", "rust_test"] },
                "path": { "type": "string", "description": "Workspace file or directory to search. Defaults to the workspace root." },
                "include": { "type": "string", "description": "Optional glob matched against workspace-relative paths." }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn summary(&self, args: &Value) -> String {
        args["query"].as_str().unwrap_or_default().to_string()
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(SEARCH_TIMEOUT + Duration::from_secs(1))
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let workspace = ctx.workspace.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            let args = parse_args(args)?;
            tokio::task::spawn_blocking(move || search(&workspace, &cancel, args))
                .await
                .map_err(|error| ToolError::Failed(format!("Structural search failed: {error}")))?
                .map(ToolOutput::text)
        })
    }
}

fn search(workspace: &Workspace, cancel: &Cancel, args: Args) -> Result<String, ToolError> {
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let root = workspace.resolve(args.path.as_deref().unwrap_or("."))?;
    let include = args.include.as_deref().map(compile_glob).transpose()?;
    let query = args.query;
    let mut files = 0;
    let mut hits = Vec::new();
    let mut parse_error = None;
    let scan = scan_text_files(
        workspace,
        cancel,
        &root,
        |path| {
            include.as_ref().is_none_or(|glob| matches(glob, path))
                && Grammar::for_path(&path.to_string_lossy())
                    .is_some_and(|grammar| query.supports(grammar))
        },
        |path, source| {
            if files >= MAX_FILES || hits.len() > MAX_GREP_MATCHES {
                return false;
            }
            files += 1;
            let Some(grammar) = Grammar::for_path(&path.to_string_lossy()) else {
                return true;
            };
            let path = crate::paths::relative_text(path);
            let limit = MAX_GREP_MATCHES + 1 - hits.len();
            match query.lines(grammar, source, limit, || {
                cancel.is_cancelled() || Instant::now() >= deadline
            }) {
                Ok(lines) => {
                    hits.extend(lines.into_iter().map(|line| (path.clone(), line)));
                    hits.len() <= MAX_GREP_MATCHES
                }
                Err(error) => {
                    parse_error = Some(error);
                    false
                }
            }
        },
    );
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut truncated = match scan {
        Ok(truncated) => truncated,
        Err(ToolError::Timeout(_)) => true,
        Err(error) => return Err(error),
    };
    if let Some(error) = parse_error
        && Instant::now() < deadline
    {
        return Err(ToolError::Failed(error));
    }
    hits.sort_unstable();
    hits.dedup();
    let mut output = format!(
        "{} matching line(s) for {}:",
        hits.len().min(MAX_GREP_MATCHES),
        query.description()
    );
    for (path, line) in hits.into_iter().take(MAX_GREP_MATCHES) {
        let row = format!("\n{path}:{line}");
        if output.len() + row.len() + TRUNCATED.len() > TOOL_OUTPUT_THRESHOLD_CHARS {
            truncated = true;
            break;
        }
        output.push_str(&row);
    }
    if truncated {
        output.push_str(TRUNCATED);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::context;

    #[test]
    fn finds_syntax_in_an_independent_workspace_and_honors_ignore_rules() {
        let (ctx, root, cancel_handle) = context("structure");
        std::fs::create_dir(root.join("ignored")).unwrap();
        std::fs::write(root.join(".ignore"), "ignored/\n").unwrap();
        std::fs::write(root.join("ignored/skip.ts"), "invoke('ignored');").unwrap();
        std::fs::write(root.join("z.tsx"), "app.invoke('member');\n").unwrap();
        std::fs::write(
            root.join("a.ts"),
            "// invoke('comment');\nconst s = \"invoke('string')\";\ninvoke('call');\n",
        )
        .unwrap();
        std::fs::write(root.join("api.rs"), "#[tauri::command]\n#[cfg(unix)]\nfn run() {}\n#[test]\nfn check() {}\n#[tauri::command]\nconst DECOY: i32 = 1;\nfn plain() {}\n").unwrap();
        let all_invokes = search(
            &ctx.workspace,
            &ctx.cancel,
            Args {
                query: Structure::InvokeCall,
                path: None,
                include: None,
            },
        )
        .unwrap();
        assert_eq!(
            all_invokes,
            "2 matching line(s) for invoke call:\na.ts:3\nz.tsx:1"
        );
        let filtered = search(
            &ctx.workspace,
            &ctx.cancel,
            Args {
                query: Structure::InvokeCall,
                path: None,
                include: Some("*.tsx".into()),
            },
        )
        .unwrap();
        assert_eq!(filtered, "1 matching line(s) for invoke call:\nz.tsx:1");
        let commands = search(
            &ctx.workspace,
            &ctx.cancel,
            Args {
                query: Structure::TauriCommand,
                path: Some("api.rs".into()),
                include: None,
            },
        )
        .unwrap();
        assert_eq!(
            commands,
            "1 matching line(s) for Tauri command definition:\napi.rs:3"
        );
        let tests = search(
            &ctx.workspace,
            &ctx.cancel,
            Args {
                query: Structure::RustTest,
                path: None,
                include: None,
            },
        )
        .unwrap();
        assert_eq!(
            tests,
            "1 matching line(s) for Rust test definition:\napi.rs:5"
        );
        cancel_handle.cancel();
        assert!(matches!(
            search(
                &ctx.workspace,
                &ctx.cancel,
                Args {
                    query: Structure::RustTest,
                    path: None,
                    include: None
                },
            ),
            Err(ToolError::Cancelled)
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
