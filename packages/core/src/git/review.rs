use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};

use crate::SharedState;
use crate::shell_env::ApplyEnvironment;
use crate::syntax::MAX_SOURCE_BYTES;
use crate::tools::{Workspace, image};

use super::{run_git, run_git_bytes, run_patch, status_name, valid_hash, valid_relative_path};
use super::{symbols, worktree_patch};

const COMMIT_DIFF: &[&str] = &[
    "diff-tree",
    "--root",
    "-r",
    "--no-commit-id",
    "--no-relative",
    "--diff-merges=first-parent",
    "--find-renames",
    "--no-ext-diff",
    "--no-textconv",
];
const STALE: &str =
    "The file changed since this diff was loaded. Refresh source control to continue.";
const UNAVAILABLE: &str = "Additional context is unavailable for these file versions.";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommitFile {
    path: String,
    old_path: Option<String>,
    status: &'static str,
    added: Option<usize>,
    removed: Option<usize>,
}

fn commit_files(workspace: &Path, hash: &str) -> Result<Vec<CommitFile>, String> {
    if !valid_hash(hash) {
        return Err("Invalid commit hash.".into());
    }
    let mut args = COMMIT_DIFF.to_vec();
    args.extend(["--raw", "--numstat", "-z", hash, "--"]);
    let output = run_git(workspace, &args)?;
    let mut records = output.split_terminator('\0').peekable();
    let mut files = Vec::new();

    while records.peek().is_some_and(|record| record.starts_with(':')) {
        let header = records.next().unwrap();
        let code = header
            .split_whitespace()
            .last()
            .ok_or("Invalid change record.")?;
        let first = records.next().ok_or("Missing file path.")?;
        let (path, old_path) = if code.starts_with(['R', 'C']) {
            (
                records.next().ok_or("Missing renamed file path.")?,
                Some(first.to_owned()),
            )
        } else {
            (first, None)
        };
        files.push(CommitFile {
            path: path.to_owned(),
            old_path,
            status: status_name(code),
            added: None,
            removed: None,
        });
    }

    let mut stats = HashMap::new();
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, '\t');
        let added = fields.next().and_then(|number| number.parse().ok());
        let removed = fields.next().and_then(|number| number.parse().ok());
        let path = match fields.next() {
            Some("") => {
                records.next().ok_or("Missing original file path.")?;
                records.next().ok_or("Missing renamed file path.")?
            }
            Some(path) => path,
            None => return Err("Invalid line counts.".into()),
        };
        stats.insert(path, (added, removed));
    }
    for file in &mut files {
        if let Some(&(added, removed)) = stats.get(file.path.as_str()) {
            file.added = added;
            file.removed = removed;
        }
    }
    Ok(files)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Target {
    path: String,
    hash: Option<String>,
    old_path: Option<String>,
    status: String,
}

impl Target {
    fn patch(&self, workspace: &Path) -> Result<String, String> {
        let Some(hash) = &self.hash else {
            return worktree_patch(workspace, &self.path, true);
        };
        if !valid_hash(hash) {
            return Err("Invalid commit hash.".into());
        }
        if !valid_relative_path(&self.path)
            || self
                .old_path
                .as_deref()
                .is_some_and(|path| !valid_relative_path(path))
        {
            return Err("Invalid file path.".into());
        }
        // Commit paths are repository-relative even when the window is opened in a subdirectory.
        let up = run_git(workspace, &["rev-parse", "--show-cdup"])?;
        let root = workspace.join(up.strip_suffix('\n').unwrap_or(&up));
        let filter = match self.status.as_str() {
            "added" => "--diff-filter=A",
            "modified" => "--diff-filter=M",
            "deleted" => "--diff-filter=D",
            "renamed" => "--diff-filter=R",
            "copied" => "--diff-filter=C",
            "typechange" => "--diff-filter=T",
            _ => return Err("Invalid commit file status.".into()),
        };
        let mut args = COMMIT_DIFF.to_vec();
        args.extend([
            "--patch",
            "--no-color",
            "--full-index",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            filter,
            hash,
            "--",
            &self.path,
        ]);
        if let Some(old_path) = &self.old_path {
            args.push(old_path);
        }
        run_patch(&root, &args, false)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    file_header: String,
    index_line: String,
}

struct Section<'a> {
    text: &'a str,
    old_id: &'a str,
    new_id: &'a str,
    old_path: Option<String>,
    new_path: Option<String>,
}

impl Snapshot {
    fn section<'a>(&self, patch: &'a str) -> Result<Section<'a>, String> {
        let (section, index_line) = patch
            .split("\ndiff --git ")
            .find_map(|section| {
                let mut lines = section.split_terminator('\n');
                let heading = lines.next()?;
                if heading.strip_prefix("diff --git ").unwrap_or(heading) != self.file_header {
                    return None;
                }
                let index = lines
                    .take_while(|line| !line.starts_with("@@"))
                    .find_map(|line| line.strip_prefix("index "))?;
                (index == self.index_line).then_some((section, index))
            })
            .ok_or(STALE)?;
        let header = section
            .split_terminator('\n')
            .skip(1)
            .take_while(|line| !line.starts_with("@@"));
        let (old_id, new_id) = index_line
            .split_whitespace()
            .next()
            .and_then(|pair| pair.split_once(".."))
            .ok_or(UNAVAILABLE)?;
        let marker = |line: &str, prefix: &str| {
            let path = patch_path(line)?;
            let path = path.strip_prefix(prefix)?;
            valid_relative_path(path).then(|| path.to_owned())
        };
        let mut paths = (None, None);
        for line in header {
            if let Some(old) = line.strip_prefix("--- ") {
                paths.0 = marker(old, "a/");
            } else if let Some(new) = line.strip_prefix("+++ ") {
                paths.1 = marker(new, "b/");
            }
        }
        Ok(Section {
            text: section,
            old_id,
            new_id,
            old_path: paths.0,
            new_path: paths.1,
        })
    }
}

impl Section<'_> {
    fn sources(&self, workspace: &Path, target: &Target) -> Result<(String, String), String> {
        let read = |id: &str, old: bool, path: Option<&str>| -> Result<String, String> {
            if missing(id) {
                return Ok(String::new());
            }
            let path = path.ok_or(UNAVAILABLE)?;
            let disk = (!old && target.hash.is_none()).then_some(path);
            let bytes = load_blob(workspace, id, disk, MAX_SOURCE_BYTES as u64)?;
            if bytes.contains(&0) {
                return Err(UNAVAILABLE.into());
            }
            let source = String::from_utf8(bytes).map_err(|_| UNAVAILABLE)?;
            patch_matches(&source, self.text, old).ok_or(UNAVAILABLE)?;
            Ok(source)
        };
        Ok((
            read(self.old_id, true, self.old_path.as_deref())?,
            read(self.new_id, false, self.new_path.as_deref())?,
        ))
    }
}

fn missing(id: &str) -> bool {
    id.bytes().all(|byte| byte == b'0')
}

fn load_blob(
    workspace: &Path,
    oid: &str,
    disk: Option<&str>,
    limit: u64,
) -> Result<Vec<u8>, String> {
    if !matches!(oid.len(), 40 | 64) || !valid_hash(oid) {
        return Err(UNAVAILABLE.into());
    }
    if let Ok(size) = run_git(workspace, &["cat-file", "-s", oid]) {
        if size.trim().parse::<u64>().map_err(|_| UNAVAILABLE)? > limit {
            return Err("The file exceeds the size limit for this preview.".into());
        }
        return run_git_bytes(workspace, &["cat-file", "blob", oid]);
    }
    let path = disk.ok_or(UNAVAILABLE)?;
    let root = Workspace::new(workspace.to_str().ok_or(UNAVAILABLE)?)?;
    let target = root.resolve(root.root().join(path).to_str().ok_or(UNAVAILABLE)?)?;
    let file = std::fs::File::open(&target).map_err(|_| STALE)?;
    let metadata = file.metadata().map_err(|_| STALE)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(UNAVAILABLE.into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| STALE)?;
    if bytes.len() as u64 > limit {
        return Err(UNAVAILABLE.into());
    }
    if hash_blob(workspace, &bytes).ok_or(UNAVAILABLE)?.trim() != oid {
        return Err(STALE.into());
    }
    Ok(bytes)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewDetails {
    old_lines: usize,
    new_lines: usize,
    symbols: symbols::DiffSymbols,
}

fn details(
    workspace: &Path,
    target: &Target,
    snapshot: &Snapshot,
) -> Result<ReviewDetails, String> {
    let patch = target.patch(workspace)?;
    let section = snapshot.section(&patch)?;
    let (old, new) = section.sources(workspace, target)?;
    Ok(ReviewDetails {
        old_lines: old.split_terminator('\n').count(),
        new_lines: new.split_terminator('\n').count(),
        symbols: symbols::outlines(
            section.old_path.as_deref(),
            &old,
            section.new_path.as_deref(),
            &new,
        ),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextRange {
    old_start: usize,
    new_start: usize,
    count: usize,
}

fn context_rows(old: &str, new: &str, range: &ContextRange) -> Result<Vec<String>, String> {
    let ContextRange {
        old_start,
        new_start,
        count,
    } = *range;
    if !(1..=40).contains(&count) || old_start == 0 || new_start == 0 {
        return Err("Invalid diff context range.".into());
    }
    let old_rows = old.split_terminator('\n').skip(old_start - 1).take(count);
    let new_rows: Vec<_> = new
        .split_terminator('\n')
        .skip(new_start - 1)
        .take(count)
        .collect();
    if new_rows.len() != count || old_rows.ne(new_rows.iter().copied()) {
        return Err(UNAVAILABLE.into());
    }
    Ok(new_rows.into_iter().map(str::to_owned).collect())
}

fn context(
    workspace: &Path,
    target: &Target,
    snapshot: &Snapshot,
    range: &ContextRange,
) -> Result<Vec<String>, String> {
    let patch = target.patch(workspace)?;
    let (old, new) = snapshot.section(&patch)?.sources(workspace, target)?;
    context_rows(&old, &new, range)
}

fn preview_image(workspace: &Path, target: &Target, snapshot: &Snapshot) -> Result<String, String> {
    let patch = target.patch(workspace)?;
    let section = snapshot.section(&patch)?;
    let (oid, disk) = if missing(section.new_id) {
        (section.old_id, None)
    } else {
        (
            section.new_id,
            target.hash.is_none().then_some(target.path.as_str()),
        )
    };
    image::data_url(&load_blob(workspace, oid, disk, image::MAX_IMAGE_BYTES)?)
}

#[tauri::command]
pub(crate) async fn git_commit_files(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    hash: String,
    workspace: Option<String>,
) -> Result<Vec<CommitFile>, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || commit_files(&workspace, &hash))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn git_review_diff(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    target: Target,
    workspace: Option<String>,
) -> Result<String, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || target.patch(&workspace))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn git_review_details(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    target: Target,
    snapshot: Snapshot,
    workspace: Option<String>,
) -> Result<ReviewDetails, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || details(&workspace, &target, &snapshot))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn git_review_context(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    target: Target,
    snapshot: Snapshot,
    range: ContextRange,
    workspace: Option<String>,
) -> Result<Vec<String>, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || context(&workspace, &target, &snapshot, &range))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn git_review_image(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    target: Target,
    snapshot: Snapshot,
    workspace: Option<String>,
) -> Result<String, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || preview_image(&workspace, &target, &snapshot))
        .await
        .map_err(|error| error.to_string())?
}

fn patch_matches(source: &str, section: &str, old: bool) -> Option<()> {
    let mut lines = source.split_terminator('\n');
    let mut consumed = 0;
    let mut row = None;
    for line in section.split_terminator('\n') {
        if let Some(header) = line.strip_prefix("@@ ") {
            let range = header.split_whitespace().nth(usize::from(!old))?;
            let start = range.get(1..)?.split(',').next()?.parse::<usize>().ok()?;
            row = Some(start);
        } else if let Some(next) = row
            && let Some(text) = line
                .strip_prefix(' ')
                .or_else(|| line.strip_prefix(if old { '-' } else { '+' }))
        {
            if lines.nth(next.checked_sub(consumed + 1)?)? != text {
                return None;
            }
            consumed = next;
            row = Some(next + 1);
        }
    }
    Some(())
}

fn hash_blob(workspace: &Path, bytes: &[u8]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(crate::paths::display_path(workspace))
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.apply_environment();
    let mut child = command.spawn().ok()?;
    let write = child.stdin.take()?.write_all(bytes);
    let output = child.wait_with_output().ok()?;
    write.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn patch_path(marker: &str) -> Option<String> {
    let marker = marker.strip_suffix('\t').unwrap_or(marker);
    let Some(quoted) = marker
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
    else {
        return Some(marker.into());
    };
    let mut path = Vec::new();
    let mut bytes = quoted.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            path.push(byte);
            continue;
        }
        path.push(match bytes.next()? {
            byte @ (b'\\' | b'"') => byte,
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            high @ b'0'..=b'3' => {
                let mid = bytes.next()?.checked_sub(b'0').filter(|digit| *digit < 8)?;
                let low = bytes.next()?.checked_sub(b'0').filter(|digit| *digit < 8)?;
                (high - b'0') * 64 + mid * 8 + low
            }
            _ => return None,
        });
    }
    String::from_utf8(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_root;

    fn commit(root: &Path, parents: &[&str]) -> String {
        let tree = run_git(root, &["write-tree"]).unwrap();
        let mut args = vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit-tree",
            tree.trim(),
            "-m",
            "Fixture",
        ];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        run_git(root, &args).unwrap().trim().to_owned()
    }

    fn target(file: &CommitFile, hash: &str) -> Target {
        Target {
            path: file.path.clone(),
            hash: Some(hash.into()),
            old_path: file.old_path.clone(),
            status: file.status.into(),
        }
    }

    fn snapshot(patch: &str) -> Snapshot {
        let find = |prefix| {
            patch
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .unwrap()
                .into()
        };
        Snapshot {
            file_header: find("diff --git "),
            index_line: find("index "),
        }
    }

    #[test]
    fn commit_review_reads_metadata_independently_and_diffs_against_the_first_parent() {
        let root = temp_root("git-review-commit");
        let git = |args: &[&str]| run_git(&root, args).unwrap();
        git(&["init", "-b", "main"]);
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/name 新.rs"), "fn before() {}\n").unwrap();
        std::fs::write(
            root.join("large.txt"),
            "line\n".repeat(super::super::MAX_PATCH_BYTES / 5),
        )
        .unwrap();
        std::fs::write(root.join("pixel.png"), b"\x89PNG\r\n\x1a\n\0pixels").unwrap();
        git(&["add", "."]);
        let initial = commit(&root, &[]);

        let workspace = root.join("nested");
        let initial_files = commit_files(&workspace, &initial).unwrap();
        assert_eq!(initial_files.len(), 3);
        let file = |path: &str| initial_files.iter().find(|file| file.path == path).unwrap();
        let small = file("nested/name 新.rs");
        assert_eq!(
            (small.status, small.added, small.removed),
            ("added", Some(1), Some(0))
        );
        let patch = target(small, &initial).patch(&workspace).unwrap();
        assert!(patch.contains("+fn before() {}"));
        assert_eq!(
            details(&workspace, &target(small, &initial), &snapshot(&patch))
                .unwrap()
                .symbols
                .new[0]
                .name,
            "before"
        );
        assert!(
            target(file("large.txt"), &initial)
                .patch(&workspace)
                .unwrap_err()
                .contains("preview limit")
        );
        let image = target(file("pixel.png"), &initial);
        assert!(file("pixel.png").added.is_none());
        let patch = image.patch(&workspace).unwrap();
        assert!(
            preview_image(&workspace, &image, &snapshot(&patch))
                .unwrap()
                .starts_with("data:image/png;base64,")
        );

        git(&["update-ref", "HEAD", &initial]);
        git(&["mv", "nested/name 新.rs", "nested/after 新.rs"]);
        git(&["rm", "pixel.png"]);
        let second = commit(&root, &[&initial]);
        let merge = commit(&root, &[&initial, &second]);
        let files = commit_files(&workspace, &merge).unwrap();
        assert_eq!(files.len(), 2);
        let renamed = files.iter().find(|file| file.status == "renamed").unwrap();
        assert_eq!(renamed.old_path.as_deref(), Some("nested/name 新.rs"));
        assert_eq!((renamed.added, renamed.removed), (Some(0), Some(0)));
        let patch = target(renamed, &merge).patch(&workspace).unwrap();
        assert!(
            patch.contains("rename from") && patch.contains("rename to"),
            "{patch}"
        );
        let deleted = target(
            files.iter().find(|file| file.status == "deleted").unwrap(),
            &merge,
        );
        let patch = deleted.patch(&workspace).unwrap();
        assert!(
            preview_image(&workspace, &deleted, &snapshot(&patch))
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(
            commit_files(&workspace, &second)
                .unwrap()
                .iter()
                .any(|file| file.path == renamed.path)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diff_details_and_context_follow_the_patch_versions_without_writing_to_the_index() {
        let root = temp_root("git-review-context");
        let git = |args: &[&str]| run_git(&root, args).unwrap();
        git(&["init", "-b", "main"]);
        let original = "fn old() {}\n".to_owned() + &"// unchanged\n".repeat(40) + "fn last() {}\n";
        let path = "new 名.rs";
        let save = |path, source: &str| std::fs::write(root.join(path), source).unwrap();
        save("old.rs", &original);
        git(&["add", "."]);
        let initial = commit(&root, &[]);
        git(&["update-ref", "HEAD", &initial]);
        git(&["mv", "old.rs", path]);
        let edited = original.replace("fn old", "fn updated");
        save(path, &edited);
        let index = std::fs::read(root.join(".git/index")).unwrap();

        let working = Target {
            path: path.into(),
            hash: None,
            old_path: None,
            status: "renamed".into(),
        };
        assert!(
            super::super::changes(&root)
                .unwrap()
                .iter()
                .any(|file| file.old_path.as_deref() == Some("old.rs"))
        );
        let patch = working.patch(&root).unwrap();
        assert!(patch.contains("rename from"), "{patch}");
        let version = snapshot(&patch);
        let read = details(&root, &working, &version).unwrap();
        assert_eq!((read.old_lines, read.new_lines), (42, 42));
        assert_eq!(read.symbols.old[0].name, "old");
        assert_eq!(read.symbols.new[0].name, "updated");
        let range = ContextRange {
            old_start: 8,
            new_start: 8,
            count: 3,
        };
        assert_eq!(
            context(&root, &working, &version, &range).unwrap(),
            ["// unchanged"; 3]
        );
        assert!(
            context_rows(
                &original,
                &edited,
                &ContextRange {
                    old_start: 1,
                    new_start: 1,
                    count: 1
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), edited);
        save(path, &edited.replace("fn last", "fn outside_hunk"));
        assert_eq!(
            context(&root, &working, &version, &range).unwrap_err(),
            STALE
        );

        save(path, &edited);
        git(&["add", "."]);
        let next = commit(&root, &[&initial]);
        let files = commit_files(&root, &next).unwrap();
        let committed = target(&files[0], &next);
        let patch = committed.patch(&root).unwrap();
        let version = snapshot(&patch);
        save(path, "unrelated worktree content\n");
        assert_eq!(
            context(&root, &committed, &version, &range).unwrap(),
            ["// unchanged"; 3]
        );
        assert_eq!(
            details(&root, &committed, &version).unwrap().symbols.new[0].name,
            "updated"
        );

        save("worker.py", "def run():\n    return 1\n");
        save(
            "Service.java",
            "class Service {\n    int run() { return 1; }\n}\n",
        );
        for path in ["worker.py", "Service.java"] {
            let target = Target {
                path: path.into(),
                hash: None,
                old_path: None,
                status: "untracked".into(),
            };
            let patch = target.patch(&root).unwrap();
            let symbols = details(&root, &target, &snapshot(&patch)).unwrap().symbols;
            assert!(symbols.old.is_empty());
            assert!(symbols.new.iter().any(|symbol| symbol.name == "run"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn worktree_review_preserves_staged_files_and_shows_mode_only_changes() {
        let root = temp_root("git-review-status");
        let git = |args: &[&str]| run_git(&root, args).unwrap();
        git(&["init", "-b", "main"]);
        std::fs::write(root.join("deleted.rs"), "fn removed() {}\n").unwrap();
        std::fs::write(root.join("mode.txt"), "same text\n").unwrap();
        git(&["add", "."]);
        let initial = commit(&root, &[]);
        git(&["update-ref", "HEAD", &initial]);

        std::fs::write(root.join("new.rs"), "fn staged() {}\n").unwrap();
        git(&["add", "new.rs"]);
        std::fs::remove_file(root.join("new.rs")).unwrap();
        std::fs::remove_file(root.join("deleted.rs")).unwrap();
        git(&["update-index", "--chmod=+x", "mode.txt"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("mode.txt"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let files = super::super::changes(&root).unwrap();
        let read = |path: &str| {
            let file = files.iter().find(|file| file.path == path).unwrap();
            let target = Target {
                path: path.into(),
                hash: None,
                old_path: None,
                status: file.status.clone(),
            };
            let patch = target.patch(&root).unwrap();
            (target, patch)
        };

        let (target, patch) = read("new.rs");
        assert!(patch.contains("+fn staged() {}"), "{patch}");
        assert_eq!(
            details(&root, &target, &snapshot(&patch))
                .unwrap()
                .symbols
                .new[0]
                .name,
            "staged"
        );
        let (target, patch) = read("deleted.rs");
        assert!(patch.contains("-fn removed() {}"), "{patch}");
        let detail = details(&root, &target, &snapshot(&patch)).unwrap();
        assert_eq!(detail.symbols.old[0].name, "removed");
        assert!(detail.symbols.new.is_empty());
        let (_, patch) = read("mode.txt");
        assert!(
            patch.contains("old mode") && !patch.contains("@@"),
            "{patch}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
