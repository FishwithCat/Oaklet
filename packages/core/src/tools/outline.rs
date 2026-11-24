use std::fs::File;
use std::io::Read;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::syntax::{self, Grammar, MAX_SOURCE_BYTES};

use super::{
    Cancel, TOOL_OUTPUT_THRESHOLD_CHARS, Tool, ToolAccess, ToolContext, ToolError, ToolOutput,
    Workspace, parse_args,
};

const MAX_SYMBOLS: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(10);
const TRUNCATED: &str = "\n[Outline truncated. Use grep or read_file for other symbols.]";

pub(super) struct FileOutline;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
}

impl Tool for FileOutline {
    fn name(&self) -> &str {
        "file_outline"
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn description(&self) -> &'static str {
        "List declarations and their inclusive line ranges in a saved Rust, JavaScript (including JSX), TypeScript (including TSX), Python, or Java workspace file."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to a saved source file in the workspace." }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use file_outline when you know a source file but need to locate its implementation. Pass the symbol's start line as read_file offset and its inclusive line count as limit."
    }

    fn summary(&self, args: &Value) -> String {
        args["path"].as_str().unwrap_or_default().to_string()
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(TIMEOUT)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let Args { path } = parse_args(args)?;
            let workspace = ctx.workspace.clone();
            let cancel = ctx.cancel.clone();
            let deadline = Instant::now() + TIMEOUT;
            tokio::task::spawn_blocking(move || file_outline(&workspace, &cancel, deadline, &path))
                .await
                .map_err(|error| ToolError::Failed(format!("Outline task failed: {error}")))?
                .map(ToolOutput::text)
        })
    }
}

fn file_outline(
    workspace: &Workspace,
    cancel: &Cancel,
    deadline: Instant,
    path: &str,
) -> Result<String, ToolError> {
    let resolved = workspace.resolve(path)?;
    let label = workspace.relative(&resolved).display().to_string();
    let grammar = Grammar::for_path(&label)
        .ok_or_else(|| ToolError::Failed(format!("Unsupported source file: {label}")))?;
    let metadata = resolved
        .metadata()
        .map_err(|error| format!("Cannot read {label}: {error}"))?;
    if !metadata.is_file() {
        return Err(ToolError::Failed(format!("Not a regular file: {label}")));
    }
    if metadata.len() > MAX_SOURCE_BYTES as u64 {
        return Err(ToolError::Failed(format!(
            "{label} exceeds the {MAX_SOURCE_BYTES}-byte outline limit."
        )));
    }
    let mut source = String::new();
    File::open(&resolved)
        .and_then(|file| {
            file.take(MAX_SOURCE_BYTES as u64 + 1)
                .read_to_string(&mut source)
        })
        .map_err(|error| format!("Cannot read {label} as UTF-8 text: {error}"))?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ToolError::Failed(format!(
            "{label} exceeds the {MAX_SOURCE_BYTES}-byte outline limit."
        )));
    }

    let result = syntax::outline(grammar, &source, MAX_SYMBOLS, || {
        cancel.is_cancelled() || Instant::now() >= deadline
    });
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(ToolError::Timeout(TIMEOUT));
    }
    let outline = result.map_err(ToolError::Failed)?;
    let mut output = format!("Outline of {label}");
    if outline.has_errors {
        output.push_str(" (syntax errors detected; results may be incomplete)");
    }
    if outline.symbols.is_empty() {
        output.push_str("\nNo declarations found.");
        return Ok(output);
    }
    for symbol in outline.symbols {
        let name = if symbol.container.is_empty() {
            symbol.name
        } else {
            format!("{} / {}", symbol.container, symbol.name)
        };
        let line = format!(
            "\n{}–{}  {} {name}",
            symbol.start_line, symbol.end_line, symbol.kind
        );
        if output.len() + line.len() + TRUNCATED.len() > TOOL_OUTPUT_THRESHOLD_CHARS {
            output.push_str(TRUNCATED);
            return Ok(output);
        }
        output.push_str(&line);
    }
    if outline.truncated {
        output.push_str(TRUNCATED);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::context;
    use crate::tools::{Access, ToolRegistry};

    #[test]
    fn outlines_declarations_and_containers_across_grammars() {
        let cases = [
            (
                "src.rs",
                "struct Café;\r\nimpl Café {\r\n  #[inline]\r\n  fn run() { helper(); }\r\n}\r\nstruct Other;\r\nimpl Other { fn run() {} }",
                vec![
                    ("Café", "struct", "", 1, 1),
                    ("run", "method", "Café", 3, 4),
                    ("Other", "struct", "", 6, 6),
                    ("run", "method", "Other", 7, 7),
                ],
            ),
            (
                "view.jsx",
                "export const View = () => <div>hello</div>;\nconst decoy = 'function bogus() {}';",
                vec![("View", "function", "", 1, 1)],
            ),
            (
                "api.ts",
                "interface Api { run(): void; }\nexport type Result = string;\nexport function run() {}",
                vec![
                    ("Api", "interface", "", 1, 1),
                    ("run", "method", "Api", 1, 1),
                    ("Result", "type", "", 2, 2),
                    ("run", "function", "", 3, 3),
                ],
            ),
            (
                "view.tsx",
                "type Props = { title: string };\nexport const View = ({ title }: Props) => (\n  <div>{title}</div>\n);",
                vec![("Props", "type", "", 1, 1), ("View", "function", "", 2, 4)],
            ),
            (
                "worker.py",
                concat!(
                    "@register\n",
                    "class Worker:\n",
                    "    @logged\n",
                    "    async def run(self):\n",
                    "        def inner():\n",
                    "            return 1\n",
                    "        return inner()\n",
                    "\n",
                    "async def top():\n",
                    "    return 2\n",
                ),
                vec![
                    ("Worker", "class", "", 1, 7),
                    ("run", "method", "Worker", 3, 7),
                    ("inner", "function", "Worker / run", 5, 6),
                    ("top", "function", "", 9, 10),
                ],
            ),
            (
                "Outer.java",
                concat!(
                    "@interface Marker { int value(); }\n",
                    "interface Action { void run(); }\n",
                    "enum Mode { ON; void stop() {} }\n",
                    "class Outer {\n",
                    "    Outer() {}\n",
                    "    record Inner(int value) {\n",
                    "        Inner {}\n",
                    "        @Deprecated\n",
                    "        int read() { return value; }\n",
                    "    }\n",
                    "}\n",
                ),
                vec![
                    ("Marker", "annotation", "", 1, 1),
                    ("value", "method", "Marker", 1, 1),
                    ("Action", "interface", "", 2, 2),
                    ("run", "method", "Action", 2, 2),
                    ("Mode", "enum", "", 3, 3),
                    ("stop", "method", "Mode", 3, 3),
                    ("Outer", "class", "", 4, 11),
                    ("Outer", "constructor", "Outer", 5, 5),
                    ("Inner", "record", "Outer", 6, 10),
                    ("Inner", "constructor", "Outer / Inner", 7, 7),
                    ("read", "method", "Outer / Inner", 8, 9),
                ],
            ),
        ];
        for (path, source, expected) in cases {
            let result = syntax::outline(Grammar::for_path(path).unwrap(), source, 200, || false)
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            assert!(!result.has_errors, "{path}");
            assert_eq!(
                result
                    .symbols
                    .iter()
                    .map(|symbol| (
                        symbol.name.as_str(),
                        symbol.kind,
                        symbol.container.as_str(),
                        symbol.start_line,
                        symbol.end_line,
                    ))
                    .collect::<Vec<_>>(),
                expected,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn planning_agent_can_read_the_implementation_from_the_outlined_lines() {
        let (ctx, root, _cancel) = context("file-outline");
        std::fs::write(
            root.join("code.rs"),
            "// header\nfn first() {}\nfn second() {\n    first();\n}\n",
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        registry.set_access(Access::planning());
        let outline = registry
            .execute(
                &ctx,
                &super::super::tests::Allow,
                "file_outline",
                "outline",
                json!({ "path": "code.rs" }),
            )
            .await;
        assert!(outline.ok, "{}", outline.output);
        assert!(outline.output.contains("3–5  function second"));
        let read = registry
            .execute(
                &ctx,
                &super::super::tests::Allow,
                "read_file",
                "read",
                json!({ "path": "code.rs", "offset": 3, "limit": 3 }),
            )
            .await;
        assert!(read.ok, "{}", read.output);
        assert_eq!(
            read.output,
            "[lines 3-5 of 5]\nfn second() {\n    first();\n}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn distinguishes_incomplete_truncated_and_unavailable_outlines() {
        let (ctx, root, _cancel) = context("outline-limits");
        let deadline = Instant::now() + TIMEOUT;
        let run = |path| file_outline(&ctx.workspace, &ctx.cancel, deadline, path);
        std::fs::write(
            root.join("partial.ts"),
            "function valid() {}\nconst broken = (",
        )
        .unwrap();
        let partial = run("partial.ts").unwrap();
        assert!(partial.contains("may be incomplete"));
        assert!(partial.contains("function valid"));
        std::fs::write(root.join("empty.rs"), "").unwrap();
        assert!(run("empty.rs").unwrap().contains("No declarations"));
        let many = (0..201)
            .map(|i| format!("fn f{i}() {{}}\n"))
            .collect::<String>();
        std::fs::write(root.join("many.rs"), many).unwrap();
        assert!(run("many.rs").unwrap().ends_with(TRUNCATED));
        assert!(
            run("missing.md")
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        assert!(
            run("../outside.rs")
                .unwrap_err()
                .to_string()
                .contains("outside the workspace")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
