//! Workspace discovery: `glob` for file paths and `grep` for file contents.
//!
//! Both walk with the `ignore` crate, so the same `.gitignore`/`.ignore` rules
//! that hide files from the user also hide them from the model. Results are
//! capped and paths stay workspace-relative; the model can narrow the next
//! search instead of reading a million matches.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolError, ToolOutput, Workspace, parse_args};

const MAX_GLOB_RESULTS: usize = 100;
const MAX_GLOB_SCAN: usize = 20_000;
const MAX_GREP_MATCHES: usize = 250;
const MAX_GREP_FILE_BYTES: u64 = 2_000_000;
const MAX_LINE_CHARS: usize = 2_000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

fn walker(root: &Path, include_hidden: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .follow_links(false)
        .require_git(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_string_lossy().as_ref(),
                    ".git" | ".svn" | ".hg" | ".jj" | ".bzr" | ".sl"
                )
        });
    builder
}

fn compile_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    if pattern.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "pattern must not be empty.".into(),
        ));
    }
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| ToolError::InvalidArguments(format!("invalid glob: {error}")))
}

fn matches(glob: &GlobMatcher, relative: &Path) -> bool {
    glob.is_match(relative)
        || relative
            .file_name()
            .is_some_and(|name| glob.is_match(Path::new(name)))
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_path<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(workspace.root()).unwrap_or(path)
}

pub struct Glob;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArgs {
    pattern: String,
    path: Option<String>,
}

impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Find files in the workspace whose paths match a glob pattern, honoring .gitignore and .ignore. Returns up to 100 workspace-relative paths, newest first."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern such as \"**/*.rs\" or \"src/**/*.test.ts\". A pattern without a slash also matches basenames at any depth." },
                "path": { "type": "string", "description": "Directory to search under. Defaults to the workspace root." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use glob to find files by name before falling back to list_dir; it honors .gitignore and searches recursively."
    }

    fn summary(&self, args: &Value) -> String {
        args.get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(SEARCH_TIMEOUT)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let workspace = ctx.workspace.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || glob(&workspace, &cancel, args)).await {
                Ok(result) => result.map(ToolOutput::text),
                Err(error) => Err(ToolError::Failed(format!("Glob task failed: {error}"))),
            }
        })
    }
}

fn glob(workspace: &Workspace, cancel: &super::Cancel, args: Value) -> Result<String, ToolError> {
    let args: GlobArgs = parse_args(args)?;
    let pattern = compile_glob(&args.pattern)?;
    let root = workspace.resolve(args.path.as_deref().unwrap_or("."))?;
    if !root.is_dir() {
        return Err(ToolError::Failed("That path is not a directory.".into()));
    }

    let mut found: Vec<(PathBuf, SystemTime)> = Vec::new();
    let mut scan_truncated = false;
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    for result in walker(&root, true).build() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ToolError::Timeout(SEARCH_TIMEOUT));
        }
        let Ok(entry) = result else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = relative_path(workspace, entry.path());
        if !matches(&pattern, relative) {
            continue;
        }
        if found.len() >= MAX_GLOB_SCAN {
            scan_truncated = true;
            break;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        found.push((relative.to_path_buf(), modified));
    }

    if found.is_empty() {
        return Ok(format!("No files match {}.", args.pattern));
    }
    let total = found.len();
    found.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    found.truncate(MAX_GLOB_RESULTS);

    let mut lines = vec![format!(
        "{} matching file(s){}:",
        total,
        if scan_truncated { " (scan capped)" } else { "" }
    )];
    lines.extend(found.into_iter().map(|(path, _)| display_path(&path)));
    if total > MAX_GLOB_RESULTS || scan_truncated {
        lines.push(format!(
            "[... more files omitted; showing at most {MAX_GLOB_RESULTS}]"
        ));
    }
    Ok(lines.join("\n"))
}

pub struct Grep;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a Rust regular expression. Returns matching lines with file paths and line numbers, honoring .gitignore and .ignore. Returns up to 250 matches."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the workspace root." },
                "include": { "type": "string", "description": "One glob filter for which files to search, such as \"*.rs\" or \"*.{ts,tsx}\"." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use grep to locate symbols, call sites, and error text; it searches recursively and respects .gitignore. Prefer it over run_command with rg/grep."
    }

    fn summary(&self, args: &Value) -> String {
        args.get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(SEARCH_TIMEOUT)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let workspace = ctx.workspace.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || grep(&workspace, &cancel, args)).await {
                Ok(result) => result.map(ToolOutput::text),
                Err(error) => Err(ToolError::Failed(format!("Grep task failed: {error}"))),
            }
        })
    }
}

fn grep(workspace: &Workspace, cancel: &super::Cancel, args: Value) -> Result<String, ToolError> {
    let args: GrepArgs = parse_args(args)?;
    if args.pattern.is_empty() {
        return Err(ToolError::InvalidArguments(
            "pattern must not be empty.".into(),
        ));
    }
    let pattern = Regex::new(&args.pattern)
        .map_err(|error| ToolError::InvalidArguments(format!("invalid regex: {error}")))?;
    let include = args
        .include
        .as_deref()
        .map(str::trim)
        .filter(|include| !include.is_empty())
        .map(compile_glob)
        .transpose()?;
    let root = workspace.resolve(args.path.as_deref().unwrap_or("."))?;

    let mut found: Vec<(String, usize, String)> = Vec::new();
    let mut limit_hit = false;
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    'walk: for result in walker(&root, false).build() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ToolError::Timeout(SEARCH_TIMEOUT));
        }
        let Ok(entry) = result else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = relative_path(workspace, entry.path());
        if include
            .as_ref()
            .is_some_and(|glob| !matches(glob, relative))
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_GREP_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        if bytes.iter().take(8_192).any(|byte| *byte == 0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if !pattern.is_match(line) {
                continue;
            }
            if found.len() >= MAX_GREP_MATCHES {
                limit_hit = true;
                break 'walk;
            }
            let line = line.trim_end_matches('\r');
            let line: String = line.chars().take(MAX_LINE_CHARS).collect();
            found.push((display_path(relative), index + 1, line));
        }
    }

    if found.is_empty() {
        return Ok(format!("No matches for {}.", args.pattern));
    }

    let mut lines = vec![format!(
        "{} match(es){}:",
        found.len(),
        if limit_hit {
            "; stopped at the match limit, narrow the search"
        } else {
            ""
        }
    )];
    let mut current = "";
    for (path, line, text) in &found {
        if path != current {
            lines.push(path.clone());
            current = path;
        }
        lines.push(format!("  {line}: {text}"));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Glob, Grep, Tool};
    use crate::tools::test_support::context;

    fn seed(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn alpha() {}\nbeta\n").unwrap();
        std::fs::write(root.join("root.rs"), "fn root() {}\n").unwrap();
        std::fs::write(root.join("src/b.ts"), "const alpha = 1;\n").unwrap();
        std::fs::write(root.join("ignored/c.rs"), "alpha\n").unwrap();
        std::fs::write(root.join("binary.bin"), b"\0alpha").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    }

    #[tokio::test]
    async fn glob_finds_files_and_honors_ignore_rules() {
        let (ctx, root, _handle) = context("glob");
        seed(&root);
        let output = Glob
            .execute(&ctx, json!({ "pattern": "**/*.rs" }))
            .await
            .unwrap();
        assert!(output.text.contains("src/a.rs"));
        assert!(output.text.contains("root.rs"));
        assert!(!output.text.contains("ignored/c.rs"));

        let output = Glob
            .execute(&ctx, json!({ "pattern": "*.ts" }))
            .await
            .unwrap();
        assert!(output.text.contains("src/b.ts"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn grep_reports_lines_and_filters_by_include() {
        let (ctx, root, _handle) = context("grep");
        seed(&root);
        let output = Grep
            .execute(&ctx, json!({ "pattern": "alpha", "include": "*.rs" }))
            .await
            .unwrap();
        assert!(output.text.contains("src/a.rs"));
        assert!(output.text.contains("1: fn alpha() {}"));
        assert!(!output.text.contains("src/b.ts"));
        assert!(!output.text.contains("ignored/c.rs"));
        assert!(!output.text.contains("binary.bin"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn invalid_patterns_and_outside_paths_are_rejected() {
        let (ctx, root, _handle) = context("search-errors");
        seed(&root);
        assert!(Grep.execute(&ctx, json!({ "pattern": "(" })).await.is_err());
        assert!(Glob.execute(&ctx, json!({ "pattern": "[" })).await.is_err());
        assert!(
            Glob.execute(&ctx, json!({ "pattern": "*", "path": "../" }))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
