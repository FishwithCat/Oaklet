use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

const DEFAULT_READ_LINES: usize = 400;
const MAX_READ_CHARS: usize = 60_000;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_FILE_BYTES: u64 = 2_000_000;
const MAX_OUTPUT_CHARS: usize = 30_000;
const MAX_CAPTURE_BYTES: u64 = 1_000_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

pub fn schemas() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 text file from the workspace. Large files can be paged with offset and limit.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, relative to the workspace." },
                        "offset": { "type": "integer", "description": "First line to read, 1-based. Defaults to 1." },
                        "limit": { "type": "integer", "description": "Maximum number of lines to read. Defaults to 400." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create or replace a file in the workspace. Parent directories are created automatically. The file is replaced as a whole, so include the complete contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, relative to the workspace." },
                        "content": { "type": "string", "description": "Complete file contents." }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Replace an exact string in a text file. Prefer this over write_file for small changes. Fails if old_string is missing or appears more than once unless replace_all is true.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, relative to the workspace." },
                        "old_string": { "type": "string", "description": "Exact text to replace." },
                        "new_string": { "type": "string", "description": "Replacement text." },
                        "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring a unique match." }
                    },
                    "required": ["path", "old_string", "new_string"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List entries in a workspace directory. Directories end with a slash.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path, relative to the workspace. Defaults to the workspace root." }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a shell command in the workspace. The user must approve each command. Returns combined stdout and stderr.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to run." }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }
    ])
}

pub async fn execute(workspace: &str, name: &str, args: &Value) -> Result<String, String> {
    let root = Path::new(workspace)
        .canonicalize()
        .map_err(|err| format!("Workspace is unavailable: {err}"))?;

    match name {
        "read_file" => read_file(&root, args),
        "write_file" => write_file(&root, args),
        "edit_file" => edit_file(&root, args),
        "list_dir" => list_dir(&root, args),
        "run_command" => run_command(&root, args).await,
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn read_file(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, string_arg(args, "path")?)?;
    let meta = std::fs::metadata(&path).map_err(|err| format!("Cannot read file: {err}"))?;
    if meta.is_dir() {
        return Err("That path is a directory; use list_dir.".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "File is {} bytes; read it in pieces with head/tail via run_command.",
            meta.len()
        ));
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("Cannot read file as UTF-8 text: {err}"))?;
    if text.is_empty() {
        return Ok("(empty file)".into());
    }

    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_LINES as u64)
        .max(1) as usize;

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
    Ok(cap(output, MAX_READ_CHARS))
}

fn write_file(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, string_arg(args, "path")?)?;
    let content = string_arg(args, "content")?;
    ensure_writable(root, &path)?;

    if path.is_dir() {
        return Err("That path is a directory.".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Cannot create parent directory: {err}"))?;
    }
    std::fs::write(&path, content).map_err(|err| format!("Cannot write file: {err}"))?;
    Ok(format!(
        "Wrote {} bytes to {}",
        content.len(),
        relative_path(root, &path).display()
    ))
}

fn edit_file(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, string_arg(args, "path")?)?;
    let old_string = string_arg(args, "old_string")?;
    let new_string = string_arg(args, "new_string")?;
    if old_string.is_empty() {
        return Err("old_string must not be empty.".into());
    }
    ensure_writable(root, &path)?;

    let meta = std::fs::metadata(&path).map_err(|err| format!("Cannot read file: {err}"))?;
    if meta.is_dir() {
        return Err("That path is a directory.".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "File is {} bytes; edit a smaller file.",
            meta.len()
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("Cannot read file as UTF-8 text: {err}"))?;

    let count = text.matches(old_string).count();
    if count == 0 {
        return Err(format!(
            "old_string was not found in {}. Read the file again and copy the exact text.",
            relative_path(root, &path).display()
        ));
    }
    let replace_all = args
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if count > 1 && !replace_all {
        return Err(format!(
            "old_string appears {count} times. Add surrounding context to make it unique, or set replace_all."
        ));
    }

    let updated = if replace_all {
        text.replace(old_string, new_string)
    } else {
        text.replacen(old_string, new_string, 1)
    };
    std::fs::write(&path, updated).map_err(|err| format!("Cannot write file: {err}"))?;
    Ok(format!(
        "Replaced {} occurrence(s) in {}",
        if replace_all { count } else { 1 },
        relative_path(root, &path).display()
    ))
}

fn ensure_writable(root: &Path, path: &Path) -> Result<(), String> {
    let relative = relative_path(root, path);
    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        return Err("Refusing to write inside .git.".into());
    }
    Ok(())
}

fn relative_path<'a>(root: &'a Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

fn list_dir(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(
        root,
        args.get("path").and_then(Value::as_str).unwrap_or("."),
    )?;
    if !path.is_dir() {
        return Err("That path is not a directory.".into());
    }

    let mut entries: Vec<String> = std::fs::read_dir(&path)
        .map_err(|err| format!("Cannot list directory: {err}"))?
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

async fn run_command(root: &Path, args: &Value) -> Result<String, String> {
    let command = string_arg(args, "command")?;
    if command.trim().is_empty() {
        return Err("Command is empty.".into());
    }

    let mut process = if cfg!(windows) {
        let mut process = tokio::process::Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = tokio::process::Command::new("sh");
        process.args(["-c", command]);
        process
    };
    process
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = process
        .spawn()
        .map_err(|err| format!("Could not start command: {err}"))?;
    let stdout = tokio::spawn(read_to_end(child.stdout.take()));
    let stderr = tokio::spawn(read_to_end(child.stderr.take()));

    let (status, timed_out) = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => (Some(status), false),
        Ok(Err(err)) => return Err(format!("Command failed to run: {err}")),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true)
        }
    };

    let stdout = stdout.await.unwrap_or_default();
    let stderr = stderr.await.unwrap_or_default();
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(&String::from_utf8_lossy(&stdout));
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&String::from_utf8_lossy(&stderr));
    }
    let output = output.trim_end();

    let summary = if timed_out {
        format!("Command timed out after {}s.", COMMAND_TIMEOUT.as_secs())
    } else {
        match status.and_then(|status| status.code()) {
            Some(0) => "Command succeeded.".to_string(),
            Some(code) => format!("Exit code {code}."),
            None => "Command terminated by a signal.".to_string(),
        }
    };

    if output.is_empty() {
        Ok(summary)
    } else {
        Ok(cap(format!("{summary}\n{output}"), MAX_OUTPUT_CHARS))
    }
}

async fn read_to_end<R>(reader: Option<R>) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(reader) = reader else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let mut limited = reader.take(MAX_CAPTURE_BYTES);
    let _ = limited.read_to_end(&mut buffer).await;
    buffer
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing string argument: {key}"))
}

fn resolve(root: &Path, input: &str) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("A path is required.".into());
    }

    let raw = Path::new(input);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };

    // Walk up to the deepest entry that exists, canonicalize it, then append the
    // missing names. This resolves symlinks (including broken ones) without
    // trusting `..` segments in the original input.
    let mut ancestor = candidate.as_path();
    let mut missing: Vec<OsString> = Vec::new();
    while ancestor.symlink_metadata().is_err() {
        let Some(parent) = ancestor.parent() else {
            return Err("Invalid path.".into());
        };
        let Some(name) = ancestor.file_name() else {
            return Err("Invalid path.".into());
        };
        missing.push(name.to_os_string());
        ancestor = parent;
    }

    let real = ancestor
        .canonicalize()
        .map_err(|err| format!("Cannot resolve path: {err}"))?;
    if !real.starts_with(root) {
        return Err(format!("Path is outside the workspace: {input}"));
    }

    let mut resolved = real;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn cap(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut capped: String = text.chars().take(max_chars).collect();
    capped.push_str("\n... [truncated]");
    capped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("oaklet-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn resolves_paths_inside_the_workspace() {
        let root = temp_root("inside");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        assert_eq!(
            resolve(&root, "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(resolve(&root, "./src/../src").unwrap(), root.join("src"));
        assert_eq!(
            resolve(&root, root.join("src/main.rs").to_str().unwrap()).unwrap(),
            root.join("src/main.rs")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_paths_outside_the_workspace() {
        let root = temp_root("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();

        assert!(resolve(&root, "../escape.txt").is_err());
        assert!(resolve(&root, "/etc/passwd").is_err());
        assert!(resolve(&root, "sub/../../escape.txt").is_err());
        assert!(resolve(&root, "..").is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reads_pages_and_writes_files() {
        let root = temp_root("rw");
        let args = json!({ "path": "src/hello.txt", "content": "one\ntwo\nthree\n" });
        write_file(&root, &args).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/hello.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );

        let page = read_file(
            &root,
            &json!({ "path": "src/hello.txt", "offset": 2, "limit": 1 }),
        )
        .unwrap();
        assert!(page.contains("two"));
        assert!(page.starts_with("[lines 2-2 of 3]"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_commands_in_the_workspace_and_reports_status() {
        let root = temp_root("command");
        let output = run_command(&root, &json!({ "command": "pwd && echo oaklet" }))
            .await
            .unwrap();
        assert!(output.contains(root.to_str().unwrap()));
        assert!(output.contains("oaklet"));

        let failed = run_command(&root, &json!({ "command": "exit 3" }))
            .await
            .unwrap();
        assert!(failed.starts_with("Exit code 3."));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn edits_files_with_unique_and_global_replacements() {
        let root = temp_root("edit");
        write_file(
            &root,
            &json!({ "path": "notes.txt", "content": "alpha beta alpha" }),
        )
        .unwrap();

        let output = edit_file(
            &root,
            &json!({ "path": "notes.txt", "old_string": "beta", "new_string": "gamma" }),
        )
        .unwrap();
        assert!(output.contains("1 occurrence"));
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "alpha gamma alpha"
        );

        let ambiguous = edit_file(
            &root,
            &json!({ "path": "notes.txt", "old_string": "alpha", "new_string": "x" }),
        );
        assert!(ambiguous.unwrap_err().contains("appears 2 times"));

        edit_file(
            &root,
            &json!({ "path": "notes.txt", "old_string": "alpha", "new_string": "x", "replace_all": true }),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "x gamma x"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_to_write_inside_git() {
        let root = temp_root("git");
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let args = json!({ "path": ".git/hooks/pre-commit", "content": "#!/bin/sh" });
        assert!(write_file(&root, &args).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_workspace() {
        let root = temp_root("symlink");
        let outside = temp_root("symlink-target");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert!(resolve(&root, "link").is_err());
        assert!(resolve(&root, "link/file.txt").is_err());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
