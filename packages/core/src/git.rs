use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use serde::Serialize;
use tauri::{State, WebviewWindow};

use crate::SharedState;
use crate::shell_env::ApplyEnvironment;
use crate::windows::workspace_for_window;

pub(crate) mod review;
mod symbols;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    hash: String,
    short_hash: String,
    author: String,
    email: String,
    date: String,
    subject: String,
    refs: Vec<String>,
    body: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLog {
    branch: String,
    branches: Vec<String>,
    commits: Vec<GitCommit>,
    has_tracked_changes: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChange {
    path: String,
    status: String,
    unstaged: bool,
    old_path: Option<String>,
}

/// Field/record separators keep subjects and multi-line bodies unambiguous.
const LOG_FORMAT: &str = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%D%x1f%b%x1e";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

fn run_git_bytes(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(workspace)
        .args(args);
    command.apply_environment();
    command.env("GIT_OPTIONAL_LOCKS", "0");
    let output = command
        .output()
        .map_err(|error| format!("Could not run git: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(if message.is_empty() {
            "git exited with an error.".to_string()
        } else {
            message.to_string()
        });
    }

    Ok(output.stdout)
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&run_git_bytes(workspace, args)?).into_owned())
}

const MAX_PATCH_BYTES: usize = 1024 * 1024;

fn run_patch(workspace: &Path, args: &[&str], allow_one: bool) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.apply_environment();
    command.env("GIT_OPTIONAL_LOCKS", "0");
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not run git: {error}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let mut bytes = Vec::new();
    let read = stdout
        .take((MAX_PATCH_BYTES + 1) as u64)
        .read_to_end(&mut bytes);
    let oversized = bytes.len() > MAX_PATCH_BYTES;
    if oversized || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let errors = errors.join().unwrap_or_default();
    read.map_err(|error| format!("Could not read git diff: {error}"))?;
    if oversized {
        return Err("Diff exceeds the 1 MiB preview limit. Inspect it in your terminal with git diff or git show.".into());
    }
    let status = status.map_err(|error| format!("Could not wait for git: {error}"))?;
    if !status.success() && !(allow_one && status.code() == Some(1)) {
        let message = String::from_utf8_lossy(&errors).trim().to_string();
        return Err(if message.is_empty() {
            "git exited with an error.".into()
        } else {
            message
        });
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn is_repository(workspace: &Path) -> bool {
    run_git(workspace, &["rev-parse", "--is-inside-work-tree"])
        .map(|text| text.trim() == "true")
        .unwrap_or(false)
}

pub fn log(workspace: &Path, include_commits: bool) -> Result<GitLog, String> {
    if !is_repository(workspace) {
        return Err("This workspace is not a git repository.".into());
    }

    let branch = run_git(workspace, &["branch", "--show-current"])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let branches = local_branches(workspace).unwrap_or_default();
    let has_tracked_changes = !run_git(
        workspace,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )?
    .is_empty();

    let commits = if include_commits {
        let format = format!("--format={LOG_FORMAT}");
        let output = match run_git(
            workspace,
            &["log", "--max-count=100", "--no-color", format.as_str()],
        ) {
            Ok(output) => output,
            // A freshly initialized repository has no HEAD to walk yet.
            Err(error) if error.contains("does not have any commits") => String::new(),
            Err(error) => return Err(error),
        };

        parse_log(&output)
    } else {
        Vec::new()
    };

    Ok(GitLog {
        branch,
        branches,
        commits,
        has_tracked_changes,
    })
}

fn local_branches(workspace: &Path) -> Result<Vec<String>, String> {
    let output = run_git(
        workspace,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ],
    )?;
    Ok(parse_branches(&output))
}

fn parse_branches(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn switch(workspace: &Path, branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.starts_with('-') {
        return Err("Invalid branch name.".into());
    }
    if !is_repository(workspace) {
        return Err("This workspace is not a git repository.".into());
    }

    let reference = format!("refs/heads/{branch}");
    if run_git(workspace, &["show-ref", "--verify", "--quiet", &reference]).is_err() {
        return Err("Branch not found.".into());
    }

    run_git(workspace, &["switch", branch])?;
    Ok(())
}

pub fn changes(workspace: &Path) -> Result<Vec<GitChange>, String> {
    if !is_repository(workspace) {
        return Err("This workspace is not a git repository.".into());
    }

    let output = run_git(
        workspace,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let prefix = run_git(workspace, &["rev-parse", "--show-prefix"])?;
    Ok(parse_status(
        &output,
        prefix.strip_suffix('\n').unwrap_or(&prefix),
    ))
}

pub fn diff(workspace: &Path, path: &str) -> Result<String, String> {
    worktree_patch(workspace, path, false)
}

fn worktree_patch(workspace: &Path, path: &str, review: bool) -> Result<String, String> {
    if !valid_relative_path(path) {
        return Err("Invalid file path.".into());
    }
    if !is_repository(workspace) {
        return Err("This workspace is not a git repository.".into());
    }

    let status = run_git(
        workspace,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            path,
        ],
    )?;
    let code = status.get(..2).unwrap_or_default();
    let rename = code
        .contains('A')
        .then(|| rename_source(workspace, path))
        .flatten()
        .filter(|original| {
            review
                || crate::syntax::Grammar::for_path(path).is_some()
                || crate::syntax::Grammar::for_path(original).is_some()
        });

    // `git diff HEAD` omits untracked files, and an added file has no HEAD
    // version to compare against. Compare those with an empty file instead.
    let options = ["--no-color", "--no-ext-diff", "--full-index"];
    let mut options = options.to_vec();
    if review {
        options.extend([
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
        ]);
    }

    if code == "??" || (code.contains('A') && rename.is_none()) {
        if workspace.join(path).symlink_metadata().is_ok() {
            let mut args = vec!["diff", "--no-index"];
            args.extend(&options);
            args.extend(["--", NULL_DEVICE, path]);
            return run_patch(workspace, &args, true);
        }

        // Added to the index and then deleted from the worktree: show the
        // still-staged version rather than a missing-file error.
        let mut args = vec!["diff", "--cached", "--relative", "--find-renames"];
        args.extend(&options);
        args.extend(["--", path]);
        return run_patch(workspace, &args, false);
    }

    let mut args = vec!["diff", "HEAD", "--relative", "--find-renames"];
    args.extend(&options);
    args.extend(["--", path]);
    if let Some(original) = &rename {
        args.push(original);
    }
    run_patch(workspace, &args, false)
}

fn rename_source(workspace: &Path, path: &str) -> Option<String> {
    let prefix = run_git(workspace, &["rev-parse", "--show-prefix"]).ok()?;
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
    let status = run_git(
        workspace,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--",
            ".",
        ],
    )
    .ok()?;
    let mut records = status.split('\0');
    while let Some(record) = records.next() {
        let bytes = record.as_bytes();
        if bytes.get(2) != Some(&b' ') {
            continue;
        }
        let code = &bytes[..2];
        if code.contains(&b'R') || code.contains(&b'C') {
            let original = records.next()?;
            if code.contains(&b'R') && record[3..].strip_prefix(prefix) == Some(path) {
                return original
                    .strip_prefix(prefix)
                    .filter(|original| valid_relative_path(original))
                    .map(str::to_owned);
            }
        }
    }
    None
}

fn valid_hash(hash: &str) -> bool {
    (4..=64).contains(&hash.len()) && hash.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn status_name(code: &str) -> &'static str {
    let bytes = code.as_bytes();
    if code == "??" {
        "untracked"
    } else if code == "DD" || code == "AA" || bytes.contains(&b'U') {
        "conflicted"
    } else if code.contains('R') {
        "renamed"
    } else if code.contains('C') {
        "copied"
    } else if code.contains('A') {
        "added"
    } else if code.contains('D') {
        "deleted"
    } else if code.contains('T') {
        "typechange"
    } else {
        "modified"
    }
}

fn parse_status(output: &str, prefix: &str) -> Vec<GitChange> {
    let mut records = output.split('\0');
    let mut changes = Vec::new();

    while let Some(record) = records.next() {
        if record.len() < 4 || record.as_bytes().get(2) != Some(&b' ') {
            continue;
        }

        let code = &record[..2];
        let original = if code.contains('R') || code.contains('C') {
            records.next()
        } else {
            None
        };
        let (path, status) = if let Some(path) = record[3..].strip_prefix(prefix) {
            (path, status_name(code))
        } else if code.contains('R') {
            let Some(path) = original.and_then(|path| path.strip_prefix(prefix)) else {
                continue;
            };
            (path, "deleted")
        } else {
            continue;
        };
        changes.push(GitChange {
            path: path.to_string(),
            status: status.to_string(),
            unstaged: code != "??" && record.as_bytes().get(1) != Some(&b' '),
            old_path: original
                .and_then(|original| original.strip_prefix(prefix))
                .filter(|original| *original != path)
                .map(str::to_owned),
        });
    }

    changes
}

fn parse_log(output: &str) -> Vec<GitCommit> {
    output
        .split('\u{1e}')
        .filter_map(|record| {
            let record = record.trim();
            if record.is_empty() {
                return None;
            }

            let mut fields = record.splitn(8, '\u{1f}');
            Some(GitCommit {
                hash: fields.next()?.to_string(),
                short_hash: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                email: fields.next()?.to_string(),
                date: fields.next()?.to_string(),
                subject: fields.next()?.to_string(),
                refs: parse_refs(fields.next()?),
                body: fields.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

fn parse_refs(field: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for entry in field.split(", ") {
        let entry = entry
            .trim()
            .strip_prefix("HEAD -> ")
            .unwrap_or(entry.trim());
        // Tags and symbolic refs such as `origin/HEAD` do not name a commit
        // directly, so only branches are useful as list markers.
        if entry.is_empty()
            || entry.starts_with("tag:")
            || entry.contains(" -> ")
            || entry.ends_with("/HEAD")
            || refs.iter().any(|existing| existing == entry)
        {
            continue;
        }
        refs.push(entry.to_string());
    }
    refs
}

#[tauri::command]
pub(crate) async fn git_log(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    include_commits: bool,
) -> Result<GitLog, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    tauri::async_runtime::spawn_blocking(move || log(&workspace, include_commits))
        .await
        .map_err(|error| format!("Could not read git history: {error}"))?
}

#[tauri::command]
pub(crate) async fn git_switch(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    branch: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    if state.chat.window_busy(&label) {
        return Err("Stop the running chat before switching branches.".into());
    }
    let workspace = workspace_for_window(&state, &label)?;
    tauri::async_runtime::spawn_blocking(move || switch(&workspace, &branch))
        .await
        .map_err(|error| format!("Could not switch branches: {error}"))?
}

#[tauri::command]
pub(crate) async fn git_changes(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<Vec<GitChange>, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    tauri::async_runtime::spawn_blocking(move || changes(&workspace))
        .await
        .map_err(|error| format!("Could not read uncommitted changes: {error}"))?
}

#[tauri::command]
pub(crate) async fn git_diff(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    path: String,
) -> Result<String, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    tauri::async_runtime::spawn_blocking(move || diff(&workspace, &path))
        .await
        .map_err(|error| format!("Could not read the file diff: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::{
        parse_branches, parse_log, parse_refs, parse_status, valid_hash, valid_relative_path,
    };

    #[test]
    fn branch_metadata_can_be_loaded_without_commit_history() {
        let root = crate::tools::test_support::temp_root("git-history");
        super::run_git(&root, &["init", "-b", "main"]).unwrap();
        super::run_git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        )
        .unwrap();
        super::run_git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]).unwrap();
        let metadata = super::log(&root, false).unwrap();
        assert_eq!(metadata.branch, "main");
        assert_eq!(metadata.branches, ["main"]);
        assert!(metadata.commits.is_empty());
        let history = super::log(&root, true).unwrap();
        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].subject, "Initial");
        assert_eq!(history.commits[0].refs, ["main", "origin/main"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn patch_preview_is_bounded_and_preserves_git_errors() {
        let root = crate::tools::test_support::temp_root("patch-limit");
        std::fs::write(root.join("small.txt"), "hello\n").unwrap();
        let args = ["diff", "--no-index", "--", super::NULL_DEVICE, "small.txt"];
        let patch = super::run_patch(&root, &args, true).unwrap();
        assert!(patch.contains("+hello"));
        assert!(super::run_patch(&root, &args, false).is_err());
        std::fs::write(
            root.join("large.txt"),
            "line\n".repeat(super::MAX_PATCH_BYTES / 5),
        )
        .unwrap();
        let error = super::run_patch(
            &root,
            &["diff", "--no-index", "--", super::NULL_DEVICE, "large.txt"],
            true,
        )
        .unwrap_err();
        assert!(error.contains("preview limit"));
        assert!(super::run_patch(&root, &["show", "invalid-ref"], false).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_commits_and_multiline_bodies() {
        let output = "abc123\u{1f}abc\u{1f}Ada\u{1f}ada@example.com\u{1f}2026-01-02T03:04:05+00:00\u{1f}Add git view\u{1f}HEAD -> main, origin/main, origin/HEAD, tag: v1.0.0\u{1f}First line\n\nSecond line\n\u{1e}
 def456\u{1f}def\u{1f}Lin\u{1f}lin@example.com\u{1f}2026-01-01T00:00:00+00:00\u{1f}Initial commit\u{1f}\u{1f}\u{1e}";

        let commits = parse_log(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc123");
        assert_eq!(commits[0].short_hash, "abc");
        assert_eq!(commits[0].author, "Ada");
        assert_eq!(commits[0].email, "ada@example.com");
        assert_eq!(commits[0].subject, "Add git view");
        assert_eq!(commits[0].refs, ["main", "origin/main"]);
        assert_eq!(commits[0].body, "First line\n\nSecond line");
        assert_eq!(commits[1].subject, "Initial commit");
        assert!(commits[1].refs.is_empty());
        assert_eq!(commits[1].body, "");
    }

    #[test]
    fn parses_only_branch_refs() {
        assert_eq!(
            parse_refs("HEAD -> main, origin/main, origin/HEAD, tag: v0.2.1"),
            ["main", "origin/main"]
        );
        assert_eq!(parse_refs("feature/nested"), ["feature/nested"]);
        assert!(parse_refs("").is_empty());
    }

    #[test]
    fn accepts_only_hex_commit_hashes() {
        assert!(valid_hash("abc123"));
        assert!(!valid_hash(""));
        assert!(!valid_hash("abc"));
        assert!(!valid_hash("--help"));
        assert!(!valid_hash(&"a".repeat(65)));
    }

    #[test]
    fn parses_working_tree_status() {
        let output =
            " M src/a.ts\0?? notes.md\0A  src/new.ts\0R  src/new-name.ts\0src/old-name.ts\0";
        let changes = parse_status(output, "");

        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].path, "src/a.ts");
        assert_eq!(changes[0].status, "modified");
        assert!(changes[0].unstaged);
        assert_eq!(changes[1].path, "notes.md");
        assert_eq!(changes[1].status, "untracked");
        assert!(!changes[1].unstaged);
        assert_eq!(changes[2].status, "added");
        assert!(!changes[2].unstaged);
        assert_eq!(changes[3].path, "src/new-name.ts");
        assert_eq!(changes[3].status, "renamed");
        assert!(!changes[3].unstaged);
    }

    #[test]
    fn subdirectory_changes_and_diffs_use_workspace_relative_paths() {
        let root = crate::tools::test_support::temp_root("git-subdirectory");
        let workspace = root.join("sub space/子目录");
        std::fs::create_dir_all(&workspace).unwrap();
        for path in ["tracked.txt", "deleted.txt", "old.txt", "move-out.txt"] {
            std::fs::write(workspace.join(path), format!("before {path}\n")).unwrap();
        }
        std::fs::write(root.join("outside.txt"), "outside\n").unwrap();
        std::fs::write(root.join("move-in.txt"), "incoming\n").unwrap();
        super::run_git(&root, &["init", "-b", "main"]).unwrap();
        super::run_git(&root, &["add", "."]).unwrap();
        super::run_git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "Fixture",
            ],
        )
        .unwrap();
        std::fs::write(workspace.join("tracked.txt"), "after\n").unwrap();
        std::fs::remove_file(workspace.join("deleted.txt")).unwrap();
        super::run_git(&workspace, &["mv", "old.txt", "new.txt"]).unwrap();
        super::run_git(&workspace, &["mv", "move-out.txt", "../../moved.txt"]).unwrap();
        super::run_git(&workspace, &["mv", "../../move-in.txt", "move-in.txt"]).unwrap();
        std::fs::write(workspace.join("untracked.txt"), "inside\n").unwrap();
        std::fs::write(root.join("outside.txt"), "external change\n").unwrap();
        let changes = super::changes(&workspace).unwrap();
        let paths: std::collections::BTreeSet<_> =
            changes.iter().map(|change| change.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "tracked.txt",
                "deleted.txt",
                "new.txt",
                "move-out.txt",
                "move-in.txt",
                "untracked.txt"
            ]
            .into_iter()
            .collect()
        );
        assert!(
            super::diff(&workspace, "tracked.txt")
                .unwrap()
                .contains("+after")
        );
        assert!(
            super::diff(&workspace, "untracked.txt")
                .unwrap()
                .contains("+inside")
        );
        assert!(
            super::diff(&workspace, "deleted.txt")
                .unwrap()
                .contains("-before deleted.txt")
        );
        assert!(
            super::diff(&workspace, "move-out.txt")
                .unwrap()
                .contains("-before move-out.txt")
        );
        assert!(
            super::diff(&workspace, "move-in.txt")
                .unwrap()
                .contains("+incoming")
        );
        assert!(
            super::diff(&workspace, "new.txt")
                .unwrap()
                .contains("+before old.txt")
        );
        assert!(
            super::diff(&workspace, "tracked.txt")
                .unwrap()
                .starts_with("diff --git a/tracked.txt b/tracked.txt")
        );
        #[cfg(unix)]
        {
            std::fs::write(workspace.join(":(top)outside.txt"), "literal filename\n").unwrap();
            assert!(
                super::diff(&workspace, ":(top)outside.txt")
                    .unwrap()
                    .contains("+literal filename")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn untracked_files_in_a_subdirectory_need_no_head() {
        let root = crate::tools::test_support::temp_root("git-subdirectory-no-head");
        let workspace = root.join("sub");
        std::fs::create_dir(&workspace).unwrap();
        super::run_git(&root, &["init"]).unwrap();
        std::fs::write(workspace.join("new.txt"), "new file\n").unwrap();
        std::fs::write(root.join("outside.txt"), "outside\n").unwrap();
        let changes = super::changes(&workspace).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "new.txt");
        assert!(!super::log(&workspace, false).unwrap().has_tracked_changes);
        assert!(
            super::diff(&workspace, &changes[0].path)
                .unwrap()
                .contains("+new file")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn branch_metadata_reports_tracked_changes_outside_the_workspace() {
        let root = crate::tools::test_support::temp_root("git-branch-outside-changes");
        let workspace = root.join("sub");
        std::fs::create_dir(&workspace).unwrap();
        super::run_git(&root, &["init"]).unwrap();
        std::fs::write(root.join("outside.txt"), "staged outside workspace\n").unwrap();
        super::run_git(&root, &["add", "outside.txt"]).unwrap();
        assert!(super::changes(&workspace).unwrap().is_empty());
        assert!(super::log(&workspace, false).unwrap().has_tracked_changes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accepts_only_workspace_relative_paths() {
        assert!(valid_relative_path("src/a.ts"));
        assert!(valid_relative_path("name with spaces.md"));
        assert!(!valid_relative_path(""));
        assert!(!valid_relative_path("/etc/passwd"));
        assert!(!valid_relative_path("../outside"));
        assert!(!valid_relative_path("src/../../outside"));
        assert!(!valid_relative_path("."));
    }

    #[test]
    fn parses_local_branches() {
        assert_eq!(
            parse_branches("feature/branch-switcher\nmain\n\n"),
            ["feature/branch-switcher", "main"]
        );
    }
}
