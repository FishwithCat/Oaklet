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

use super::{Tool, ToolAccess, ToolContext, ToolError, ToolOutput, Workspace, parse_args};

const MAX_GLOB_RESULTS: usize = 100;
const MAX_GLOB_SCAN: usize = 20_000;
pub(super) const MAX_GREP_MATCHES: usize = 250;
const MAX_GREP_PER_FILE: usize = 20;
const MAX_GREP_FILES: usize = 100;
const MAX_GREP_FILE_BYTES: u64 = 2_000_000;
const MAX_LINE_CHARS: usize = 2_000;
pub(super) const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn walker(root: &Path, include_hidden: bool) -> WalkBuilder {
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

pub(super) fn compile_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
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

pub(super) fn matches(glob: &GlobMatcher, relative: &Path) -> bool {
    glob.is_match(relative)
        || relative
            .file_name()
            .is_some_and(|name| glob.is_match(Path::new(name)))
}

pub(super) fn display_path(path: &Path) -> String {
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

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GrepMode {
    #[default]
    Content,
    Files,
    Count,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    #[serde(default)]
    mode: GrepMode,
}

impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::ReadOnly
    }

    fn description(&self) -> &'static str {
        "Search file contents with a Rust regular expression, honoring .gitignore and .ignore. Modes: \"content\" returns matching lines (up to 250, at most 20 per file), \"files\" returns matching file paths (up to 100), and \"count\" returns the match count per file (up to 100 files)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the workspace root." },
                "include": { "type": "string", "description": "One glob filter for which files to search, such as \"*.rs\" or \"*.{ts,tsx}\"." },
                "mode": {
                    "type": "string",
                    "enum": ["content", "files", "count"],
                    "description": "Output mode: \"content\" (matching lines, default), \"files\" (unique matching paths), or \"count\" (matching lines per file)."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use grep to locate symbols, call sites, and error text; it searches recursively and respects .gitignore. Prefer it over run_command with rg/grep. For a broad search where you only need to know which files are involved, pass mode: \"files\" (e.g. {\"pattern\": \"fn main\", \"mode\": \"files\"}); use mode: \"count\" to see where matches cluster, then narrow with include or path."
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

struct FileHits {
    path: String,
    lines: Vec<(usize, String)>,
    total: usize,
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
    let mode = args.mode;

    let mut files: Vec<FileHits> = Vec::new();
    let mut shown = 0usize;
    let mut truncated = false;
    scan_text_files(
        workspace,
        cancel,
        &root,
        |relative| include.as_ref().is_none_or(|glob| matches(glob, relative)),
        |relative, text| {
            let budget = if mode == GrepMode::Content {
                MAX_GREP_PER_FILE.min(MAX_GREP_MATCHES.saturating_sub(shown))
            } else {
                0
            };
            let mut hits = FileHits {
                path: display_path(relative),
                lines: Vec::new(),
                total: 0,
            };
            for (index, line) in text.lines().enumerate() {
                if !pattern.is_match(line) {
                    continue;
                }
                hits.total += 1;
                if hits.lines.len() < budget {
                    hits.lines
                        .push((index + 1, line.chars().take(MAX_LINE_CHARS).collect()));
                }
            }
            if hits.total == 0 {
                return true;
            }
            match mode {
                GrepMode::Content => {
                    if shown >= MAX_GREP_MATCHES {
                        truncated = true;
                        return false;
                    }
                    shown += hits.lines.len();
                    files.push(hits);
                }
                GrepMode::Files | GrepMode::Count => {
                    if files.len() >= MAX_GREP_FILES {
                        truncated = true;
                        return false;
                    }
                    files.push(hits);
                }
            }
            true
        },
    )?;

    if files.is_empty() {
        return Ok(match mode {
            GrepMode::Content => format!("No matches for {}.", args.pattern),
            GrepMode::Files => format!("No files match {}.", args.pattern),
            GrepMode::Count => format!("No matches in any file for {}.", args.pattern),
        });
    }

    match mode {
        GrepMode::Content => {
            let mut lines = vec![format!(
                "{} match(es){}:",
                shown,
                if truncated {
                    "; stopped at the match limit, narrow the search"
                } else {
                    ""
                }
            )];
            for file in &files {
                lines.push(file.path.clone());
                for (line, text) in &file.lines {
                    lines.push(format!("  {line}: {text}"));
                }
                let hidden = file.total - file.lines.len();
                if hidden > 0 {
                    lines.push(format!(
                        "[... {hidden} more matches in this file; narrow with include/path]"
                    ));
                }
            }
            Ok(lines.join("\n"))
        }
        GrepMode::Files => {
            let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
            paths.sort_unstable();
            paths.dedup();
            let mut lines = vec![format!("{} file(s) match:", paths.len())];
            lines.extend(paths.iter().map(|path| (*path).to_string()));
            if truncated {
                lines.push(format!(
                    "[... more files omitted; showing at most {MAX_GREP_FILES}]"
                ));
            }
            Ok(lines.join("\n"))
        }
        GrepMode::Count => {
            let mut counts: Vec<(&str, usize)> = files
                .iter()
                .map(|file| (file.path.as_str(), file.total))
                .collect();
            counts.sort_unstable_by(|left, right| left.0.cmp(right.0));
            let mut lines = vec![format!("{} file(s) with matches:", counts.len())];
            lines.extend(
                counts
                    .iter()
                    .map(|(path, count)| format!("{path}: {count}")),
            );
            if truncated {
                lines.push(format!(
                    "[... more files omitted; showing at most {MAX_GREP_FILES}]"
                ));
            }
            Ok(lines.join("\n"))
        }
    }
}

/// Visits searchable UTF-8 files until the visitor returns false.
pub(super) fn scan_text_files(
    workspace: &Workspace,
    cancel: &super::Cancel,
    root: &Path,
    accepts: impl Fn(&Path) -> bool,
    mut visit: impl FnMut(&Path, &str) -> bool,
) -> Result<bool, ToolError> {
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    for result in walker(root, false).sort_by_file_path(Path::cmp).build() {
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
        if !accepts(relative) {
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
        if !visit(relative, &text) {
            return Ok(true);
        }
    }
    Ok(false)
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
    async fn grep_files_mode_lists_unique_paths() {
        let (ctx, root, _handle) = context("grep-files");
        seed(&root);
        std::fs::write(root.join("src/many.rs"), "alpha\nalpha\nalpha\n").unwrap();
        let output = Grep
            .execute(&ctx, json!({ "pattern": "alpha", "mode": "files" }))
            .await
            .unwrap();
        assert!(output.text.starts_with("3 file(s) match:"));
        assert_eq!(output.text.matches("src/many.rs").count(), 1);
        assert!(output.text.contains("src/a.rs"));
        assert!(output.text.contains("src/b.ts"));
        assert!(!output.text.contains("ignored/c.rs"));

        let scoped = Grep
            .execute(
                &ctx,
                json!({ "pattern": "alpha", "mode": "files", "path": "src/a.rs" }),
            )
            .await
            .unwrap();
        assert!(scoped.text.contains("src/a.rs"));
        assert!(!scoped.text.contains("src/b.ts"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn grep_count_mode_reports_per_file_counts() {
        let (ctx, root, _handle) = context("grep-count");
        seed(&root);
        std::fs::write(root.join("src/many.rs"), "alpha\nalpha\nalpha\n").unwrap();
        let output = Grep
            .execute(&ctx, json!({ "pattern": "alpha", "mode": "count" }))
            .await
            .unwrap();
        assert!(output.text.starts_with("3 file(s) with matches:"));
        assert!(output.text.contains("src/a.rs: 1"));
        assert!(output.text.contains("src/b.ts: 1"));
        assert!(output.text.contains("src/many.rs: 3"));
        let a = output.text.find("src/a.rs: 1").unwrap();
        let b = output.text.find("src/b.ts: 1").unwrap();
        let many = output.text.find("src/many.rs: 3").unwrap();
        assert!(a < b && b < many);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn grep_content_mode_caps_matches_per_file() {
        let (ctx, root, _handle) = context("grep-per-file");
        seed(&root);
        std::fs::write(root.join("many.rs"), "alpha\n".repeat(25)).unwrap();
        let output = Grep
            .execute(&ctx, json!({ "pattern": "alpha", "path": "many.rs" }))
            .await
            .unwrap();
        assert!(output.text.starts_with("20 match(es):"));
        assert_eq!(output.text.matches(": alpha").count(), 20);
        assert!(
            output
                .text
                .contains("[... 5 more matches in this file; narrow with include/path]")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn grep_modes_have_distinct_empty_results() {
        let (ctx, root, _handle) = context("grep-empty");
        seed(&root);
        let content = Grep
            .execute(&ctx, json!({ "pattern": "zzz" }))
            .await
            .unwrap();
        let files = Grep
            .execute(&ctx, json!({ "pattern": "zzz", "mode": "files" }))
            .await
            .unwrap();
        let count = Grep
            .execute(&ctx, json!({ "pattern": "zzz", "mode": "count" }))
            .await
            .unwrap();
        assert_eq!(content.text, "No matches for zzz.");
        assert_eq!(files.text, "No files match zzz.");
        assert_eq!(count.text, "No matches in any file for zzz.");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn grep_rejects_unknown_mode() {
        let (ctx, root, _handle) = context("grep-mode-error");
        seed(&root);
        assert!(
            Grep.execute(&ctx, json!({ "pattern": "alpha", "mode": "bogus" }))
                .await
                .is_err()
        );
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
