use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::diff::{FileDiff, compute_diffs};
use super::{Tool, ToolContext, ToolError, ToolOutput, Workspace, parse_args};

const DEFAULT_READ_LINES: usize = 400;
const MAX_LIST_ENTRIES: usize = 500;
pub(super) const MAX_FILE_BYTES: u64 = 2_000_000;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace or create a text file through a same-directory staging file.
///
/// The destination is untouched until the staging file is fully written and
/// synced, then published with one rename (replace) or hard link (create), so
/// a failed write cannot truncate the previous contents. POSIX rename supplies
/// the replacement; Windows support would need `ReplaceFileW`.
pub(super) fn write_atomic(path: &Path, content: &str, create: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create parent directory: {error}"))?;
    }
    let existing = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file());
    if existing
        .as_ref()
        .is_some_and(|metadata| metadata.permissions().readonly())
    {
        return Err("file is read-only".into());
    }
    let (temp, mut file) = create_staging_file(path, existing.is_some())?;
    let staged = (|| {
        file.write_all(content.as_bytes())?;
        if let Some(metadata) = &existing {
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    drop(file);
    if let Err(error) = staged {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot write temporary file: {error}"));
    }
    let published = if create {
        std::fs::hard_link(&temp, path).map(|()| {
            let _ = std::fs::remove_file(&temp);
        })
    } else {
        std::fs::rename(&temp, path)
    };
    if let Err(error) = published {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "cannot {} file: {error}",
            if create { "create" } else { "replace" },
        ));
    }
    Ok(())
}

fn create_staging_file(path: &Path, replacing: bool) -> Result<(PathBuf, std::fs::File), String> {
    #[cfg(not(unix))]
    let _ = replacing;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    // Keep the staging name well below NAME_MAX even for long source names.
    let stem: String = name.chars().take(32).collect();
    for _ in 0..100 {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{stem}.{}.{counter}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Existing modes are restored after the write; new files keep the
            // process umask so replacement does not silently change permissions.
            options.mode(if replacing { 0o600 } else { 0o666 });
        }
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create temporary file: {error}")),
        }
    }
    Err("cannot create a unique temporary file".into())
}

fn path_arg(args: &Value) -> String {
    args.get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file from the workspace. Large files can be paged with offset and limit."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace." },
                "offset": { "type": "integer", "description": "First line to read, 1-based. Defaults to 1." },
                "limit": { "type": "integer", "description": "Maximum number of lines to read. Defaults to 400." }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Read a file before editing it. Use offset and limit to page through large files."
    }

    fn summary(&self, args: &Value) -> String {
        path_arg(args)
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let workspace = ctx.workspace.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || read_file(&workspace, args)).await {
                Ok(result) => result.map(ToolOutput::text),
                Err(error) => Err(ToolError::Failed(format!("Read task failed: {error}"))),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

fn read_file(workspace: &Workspace, args: Value) -> Result<String, ToolError> {
    let args: ReadFileArgs = parse_args(args)?;
    let path = workspace.resolve(&args.path)?;
    let meta = std::fs::metadata(&path).map_err(|error| format!("Cannot read file: {error}"))?;
    if meta.is_dir() {
        return Err(ToolError::Failed(
            "That path is a directory; use list_dir.".into(),
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ToolError::Failed(format!(
            "File is {} bytes; read it in pieces with head/tail via run_command.",
            meta.len()
        )));
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("Cannot read file as UTF-8 text: {error}"))?;
    if text.is_empty() {
        return Ok("(empty file)".into());
    }

    let offset = args.offset.unwrap_or(1).max(1);
    let limit = args.limit.unwrap_or(DEFAULT_READ_LINES).max(1);
    let lines: Vec<&str> = text.lines().collect();
    let start = (offset - 1).min(lines.len());
    let end = (start + limit).min(lines.len());
    let mut output = lines[start..end].join("\n");
    if start > 0 || end < lines.len() {
        output = format!(
            "[lines {}-{} of {}]\n{}",
            start + 1,
            end,
            lines.len(),
            output
        );
    }
    Ok(output)
}

pub struct WriteFile;

impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create or replace a file in the workspace. Parent directories are created automatically. The file is replaced as a whole, so include the complete contents."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace." },
                "content": { "type": "string", "description": "Complete file contents." }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "write_file replaces the whole file; include its complete contents."
    }

    fn summary(&self, args: &Value) -> String {
        path_arg(args)
    }

    fn preview(&self, ctx: &ToolContext, args: &Value) -> Vec<FileDiff> {
        let Ok(args) = parse_args::<WriteFileArgs>(args.clone()) else {
            return Vec::new();
        };
        let Ok(path) = ctx.workspace.resolve(&args.path) else {
            return Vec::new();
        };
        let existed = path.is_file();
        let before = existed
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten();
        if existed && before.is_none() {
            return Vec::new();
        }
        compute_diffs(
            &ctx.workspace.relative(&path).display().to_string(),
            before.as_deref(),
            &args.content,
        )
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { write_file(ctx, args) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    path: String,
    content: String,
}

fn write_file(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
    let args: WriteFileArgs = parse_args(args)?;
    let path = ctx.workspace.resolve(&args.path)?;
    ctx.workspace.ensure_writable(&path)?;

    if path.is_dir() {
        return Err(ToolError::Failed("That path is a directory.".into()));
    }
    let existed = path.is_file();
    let before = existed
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten();
    write_atomic(&path, &args.content, false)
        .map_err(|error| format!("Cannot write file: {error}"))?;
    let display = ctx.workspace.relative(&path).display().to_string();
    let diffs = if existed && before.is_none() {
        Vec::new()
    } else {
        compute_diffs(&display, before.as_deref(), &args.content)
    };
    Ok(ToolOutput::with_diffs(
        format!("Wrote {} bytes to {}", args.content.len(), display),
        diffs,
    ))
}

pub struct EditFile;

impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in a text file. Prefer this over write_file for small changes. Fails if old_string is missing or appears more than once unless replace_all is true."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring a unique match." }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Prefer edit_file for small changes; old_string must match the file exactly."
    }

    fn summary(&self, args: &Value) -> String {
        path_arg(args)
    }

    fn preview(&self, ctx: &ToolContext, args: &Value) -> Vec<FileDiff> {
        let Ok(args) = parse_args::<EditFileArgs>(args.clone()) else {
            return Vec::new();
        };
        let Ok(path) = ctx.workspace.resolve(&args.path) else {
            return Vec::new();
        };
        vec![FileDiff {
            path: ctx.workspace.relative(&path).display().to_string(),
            old_text: (!args.old_string.is_empty()).then_some(args.old_string),
            new_text: args.new_string,
        }]
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { edit_file(ctx, args) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

fn edit_file(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
    let args: EditFileArgs = parse_args(args)?;
    if args.old_string.is_empty() {
        return Err(ToolError::InvalidArguments(
            "old_string must not be empty.".into(),
        ));
    }
    let path = ctx.workspace.resolve(&args.path)?;
    ctx.workspace.ensure_writable(&path)?;

    let meta = std::fs::metadata(&path).map_err(|error| format!("Cannot read file: {error}"))?;
    if meta.is_dir() {
        return Err(ToolError::Failed("That path is a directory.".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ToolError::Failed(format!(
            "File is {} bytes; edit a smaller file.",
            meta.len()
        )));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("Cannot read file as UTF-8 text: {error}"))?;

    let count = text.matches(args.old_string.as_str()).count();
    if count == 0 {
        return Err(ToolError::Failed(format!(
            "old_string was not found in {}. Read the file again and copy the exact text.",
            ctx.workspace.relative(&path).display()
        )));
    }
    if count > 1 && !args.replace_all {
        return Err(ToolError::Failed(format!(
            "old_string appears {count} times. Add surrounding context to make it unique, or set replace_all."
        )));
    }

    let updated = if args.replace_all {
        text.replace(args.old_string.as_str(), &args.new_string)
    } else {
        text.replacen(args.old_string.as_str(), &args.new_string, 1)
    };
    write_atomic(&path, &updated, false).map_err(|error| format!("Cannot write file: {error}"))?;
    let display = ctx.workspace.relative(&path).display().to_string();
    let diffs = compute_diffs(&display, Some(&text), &updated);
    Ok(ToolOutput::with_diffs(
        format!(
            "Replaced {} occurrence(s) in {}",
            if args.replace_all { count } else { 1 },
            display
        ),
        diffs,
    ))
}

pub struct ListDir;

impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List entries in a workspace directory. Directories end with a slash."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path, relative to the workspace. Defaults to the workspace root." }
            },
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use list_dir to inspect a directory before reading paths."
    }

    fn summary(&self, args: &Value) -> String {
        let path = path_arg(args);
        if path.is_empty() { ".".into() } else { path }
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let workspace = ctx.workspace.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || list_dir(&workspace, args)).await {
                Ok(result) => result.map(ToolOutput::text),
                Err(error) => Err(ToolError::Failed(format!("List task failed: {error}"))),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirArgs {
    path: Option<String>,
}

fn list_dir(workspace: &Workspace, args: Value) -> Result<String, ToolError> {
    let args: ListDirArgs = parse_args(args)?;
    let path = workspace.resolve(args.path.as_deref().unwrap_or("."))?;
    if !path.is_dir() {
        return Err(ToolError::Failed("That path is not a directory.".into()));
    }

    let mut entries: Vec<String> = std::fs::read_dir(&path)
        .map_err(|error| format!("Cannot list directory: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    entries.sort_by_key(|entry| entry.to_lowercase());

    let total = entries.len();
    entries.truncate(MAX_LIST_ENTRIES);
    if entries.is_empty() {
        return Ok("(empty directory)".into());
    }
    if total > entries.len() {
        entries.push(format!("... [{} more entries]", total - entries.len()));
    }
    Ok(entries.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{edit_file, list_dir, read_file, write_atomic, write_file};
    use crate::tools::test_support::context;

    #[test]
    fn reads_pages_and_writes_files() {
        let (ctx, root, _handle) = context("rw");
        let args = json!({ "path": "src/hello.txt", "content": "one\ntwo\nthree\n" });
        write_file(&ctx, args).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/hello.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );

        let page = read_file(
            &ctx.workspace,
            json!({ "path": "src/hello.txt", "offset": 2, "limit": 1 }),
        )
        .unwrap();
        assert!(page.contains("two"));
        assert!(page.starts_with("[lines 2-2 of 3]"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn edits_files_with_unique_and_global_replacements() {
        let (ctx, root, _handle) = context("edit");
        write_file(
            &ctx,
            json!({ "path": "notes.txt", "content": "alpha beta alpha" }),
        )
        .unwrap();

        let output = edit_file(
            &ctx,
            json!({ "path": "notes.txt", "old_string": "beta", "new_string": "gamma" }),
        )
        .unwrap();
        assert!(output.contains("1 occurrence"));
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "alpha gamma alpha"
        );

        let ambiguous = edit_file(
            &ctx,
            json!({ "path": "notes.txt", "old_string": "alpha", "new_string": "x" }),
        );
        assert!(
            ambiguous
                .unwrap_err()
                .to_string()
                .contains("appears 2 times")
        );

        edit_file(
            &ctx,
            json!({ "path": "notes.txt", "old_string": "alpha", "new_string": "x", "replace_all": true }),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "x gamma x"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn attaches_applied_diffs_to_writes_and_edits() {
        let (ctx, root, _handle) = context("diffs");
        let created = write_file(
            &ctx,
            json!({ "path": "notes.txt", "content": "alpha\nbeta\n" }),
        )
        .unwrap();
        assert_eq!(created.diffs.len(), 1);
        assert_eq!(created.diffs[0].path, "notes.txt");
        assert_eq!(created.diffs[0].old_text, None);
        assert_eq!(created.diffs[0].new_text, "alpha\nbeta");

        let edited = edit_file(
            &ctx,
            json!({ "path": "notes.txt", "old_string": "beta", "new_string": "gamma" }),
        )
        .unwrap();
        assert_eq!(edited.diffs.len(), 1);
        assert_eq!(edited.diffs[0].old_text.as_deref(), Some("alpha\nbeta"));
        assert_eq!(edited.diffs[0].new_text, "alpha\ngamma");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lists_directories_with_a_slash() {
        let (ctx, root, _handle) = context("list");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("notes.txt"), "hi").unwrap();

        let output = list_dir(&ctx.workspace, json!({})).unwrap();
        assert!(output.contains("src/"));
        assert!(output.contains("notes.txt"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_unknown_and_missing_arguments() {
        let (ctx, root, _handle) = context("args");
        let unknown = read_file(&ctx.workspace, json!({ "path": "x", "nope": 1 })).unwrap_err();
        assert!(unknown.to_string().contains("unknown field"));
        let missing = read_file(&ctx.workspace, json!({})).unwrap_err();
        assert!(missing.to_string().contains("missing field"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_to_write_inside_git() {
        let (ctx, root, _handle) = context("git");
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let args = json!({ "path": ".git/hooks/pre-commit", "content": "#!/bin/sh" });
        assert!(write_file(&ctx, args).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn atomic_write_replaces_content_and_preserves_mode() {
        let (_ctx, root, _handle) = context("atomic-replace");
        let path = root.join("notes.txt");
        write_atomic(&path, "old\n", false).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }

        write_atomic(&path, "new\n", false).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640);
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn atomic_write_create_refuses_existing_and_cleans_up_failed_publish() {
        let (_ctx, root, _handle) = context("atomic-create");
        let created = root.join("created.txt");
        write_atomic(&created, "created\n", true).unwrap();
        assert_eq!(std::fs::read_to_string(&created).unwrap(), "created\n");

        assert!(write_atomic(&created, "replacement\n", true).is_err());
        assert_eq!(std::fs::read_to_string(&created).unwrap(), "created\n");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);

        let directory = root.join("directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(write_atomic(&directory, "not a file\n", false).is_err());
        assert!(directory.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_a_read_only_target() {
        use std::os::unix::fs::PermissionsExt;

        let (_ctx, root, _handle) = context("atomic-read-only");
        let path = root.join("notes.txt");
        std::fs::write(&path, "keep\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

        assert!(write_atomic(&path, "lost\n", false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep\n");
        std::fs::remove_dir_all(&root).ok();
    }
}
