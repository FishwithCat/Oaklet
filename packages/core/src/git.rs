use std::path::Path;
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
    commits: Vec<GitCommit>,
}

/// Field/record separators keep subjects and multi-line bodies unambiguous.
const LOG_FORMAT: &str = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b%x1e";

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

pub fn log(workspace: &Path) -> Result<GitLog, String> {
    let inside = run_git(workspace, &["rev-parse", "--is-inside-work-tree"])
        .map(|text| text.trim() == "true")
        .unwrap_or(false);
    if !inside {
        return Err("This workspace is not a git repository.".into());
    }

    let branch = run_git(workspace, &["branch", "--show-current"])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();

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
        commits: parse_log(&output),
    })
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
    use super::{parse_log, valid_hash};

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
}
