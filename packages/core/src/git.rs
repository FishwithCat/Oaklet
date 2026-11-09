use std::path::{Component, Path};
use std::process::Command;

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    hash: String,
    short_hash: String,
    author: String,
    email: String,
    date: String,
    subject: String,
    body: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLog {
    branch: String,
    branches: Vec<String>,
    commits: Vec<GitCommit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChange {
    path: String,
    status: String,
}

/// Field/record separators keep subjects and multi-line bodies unambiguous.
const LOG_FORMAT: &str = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b%x1e";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

fn run_git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
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

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_git_allow_one(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("Could not run git: {error}"))?;

    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(if message.is_empty() {
            "git exited with an error.".to_string()
        } else {
            message.to_string()
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn is_repository(workspace: &Path) -> bool {
    run_git(workspace, &["rev-parse", "--is-inside-work-tree"])
        .map(|text| text.trim() == "true")
        .unwrap_or(false)
}

pub fn log(workspace: &Path) -> Result<GitLog, String> {
    if !is_repository(workspace) {
        return Err("This workspace is not a git repository.".into());
    }

    let branch = run_git(workspace, &["branch", "--show-current"])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let branches = local_branches(workspace).unwrap_or_default();

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

    Ok(GitLog {
        branch,
        branches,
        commits: parse_log(&output),
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
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(parse_status(&output))
}

pub fn diff(workspace: &Path, path: &str) -> Result<String, String> {
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

    // `git diff HEAD` omits untracked files, and an added file has no HEAD
    // version to compare against. Compare those with an empty file instead.
    if code == "??" || code.contains('A') {
        if workspace.join(path).symlink_metadata().is_ok() {
            return run_git_allow_one(
                workspace,
                &[
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--no-ext-diff",
                    "--",
                    NULL_DEVICE,
                    path,
                ],
            );
        }

        // Added to the index and then deleted from the worktree: show the
        // still-staged version rather than a missing-file error.
        return run_git(
            workspace,
            &[
                "diff",
                "--cached",
                "--no-color",
                "--no-ext-diff",
                "--find-renames",
                "--",
                path,
            ],
        );
    }

    run_git(
        workspace,
        &[
            "diff",
            "HEAD",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "--",
            path,
        ],
    )
}

pub fn show(workspace: &Path, hash: &str) -> Result<String, String> {
    if !valid_hash(hash) {
        return Err("Invalid commit hash.".into());
    }

    run_git(
        workspace,
        &[
            "show",
            "--format=",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--first-parent",
            hash,
        ],
    )
}

pub fn ahead(workspace: &Path) -> Option<u32> {
    run_git(workspace, &["rev-list", "--count", "@{upstream}..HEAD"])
        .ok()?
        .trim()
        .parse()
        .ok()
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

fn parse_status(output: &str) -> Vec<GitChange> {
    let mut records = output.split('\0');
    let mut changes = Vec::new();

    while let Some(record) = records.next() {
        if record.len() < 4 || record.as_bytes().get(2) != Some(&b' ') {
            continue;
        }

        let code = &record[..2];
        changes.push(GitChange {
            path: record[3..].to_string(),
            status: status_name(code).to_string(),
        });

        // Rename/copy records include the original path as a second NUL field.
        if code.contains('R') || code.contains('C') {
            records.next();
        }
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

            let mut fields = record.splitn(7, '\u{1f}');
            Some(GitCommit {
                hash: fields.next()?.to_string(),
                short_hash: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                email: fields.next()?.to_string(),
                date: fields.next()?.to_string(),
                subject: fields.next()?.to_string(),
                body: fields.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_branches, parse_log, parse_status, valid_hash, valid_relative_path};

    #[test]
    fn parses_commits_and_multiline_bodies() {
        let output = "abc123\u{1f}abc\u{1f}Ada\u{1f}ada@example.com\u{1f}2026-01-02T03:04:05+00:00\u{1f}Add git view\u{1f}First line\n\nSecond line\n\u{1e}
def456\u{1f}def\u{1f}Lin\u{1f}lin@example.com\u{1f}2026-01-01T00:00:00+00:00\u{1f}Initial commit\u{1f}\u{1e}";

        let commits = parse_log(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc123");
        assert_eq!(commits[0].short_hash, "abc");
        assert_eq!(commits[0].author, "Ada");
        assert_eq!(commits[0].email, "ada@example.com");
        assert_eq!(commits[0].subject, "Add git view");
        assert_eq!(commits[0].body, "First line\n\nSecond line");
        assert_eq!(commits[1].subject, "Initial commit");
        assert_eq!(commits[1].body, "");
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
        let changes = parse_status(output);

        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].path, "src/a.ts");
        assert_eq!(changes[0].status, "modified");
        assert_eq!(changes[1].path, "notes.md");
        assert_eq!(changes[1].status, "untracked");
        assert_eq!(changes[2].status, "added");
        assert_eq!(changes[3].path, "src/new-name.ts");
        assert_eq!(changes[3].status, "renamed");
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
