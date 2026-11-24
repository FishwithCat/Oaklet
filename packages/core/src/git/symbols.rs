use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::shell_env::ApplyEnvironment;
use crate::syntax::{self, Grammar, MAX_SOURCE_BYTES, Symbol};
use crate::tools::Workspace;

use super::{diff, run_git, run_git_bytes, show, valid_relative_path};

#[derive(Serialize)]
pub(crate) struct DiffSymbols {
    old: Vec<Symbol>,
    new: Vec<Symbol>,
}

pub(super) fn for_diff(
    workspace: &Path,
    path: Option<&str>,
    hash: Option<&str>,
    file_index: usize,
    file_header: &str,
    index_line: &str,
) -> Result<Option<DiffSymbols>, String> {
    let (patch, prefix) = match (path, hash) {
        (Some(path), None) => (diff(workspace, path)?, String::new()),
        (None, Some(hash)) => (
            show(workspace, hash)?,
            run_git(workspace, &["rev-parse", "--show-prefix"])?,
        ),
        _ => return Err("Select a file or a commit to read diff symbols.".into()),
    };
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
    Ok(annotate(
        workspace,
        &patch,
        prefix,
        hash.is_none(),
        file_index,
        file_header,
        index_line,
    ))
}

fn annotate(
    workspace: &Path,
    patch: &str,
    prefix: &str,
    worktree: bool,
    file_index: usize,
    file_header: &str,
    index_line: &str,
) -> Option<DiffSymbols> {
    let mut sections = patch.split("\ndiff --git ");
    let section = sections.nth(file_index)?;
    let heading = section.lines().next()?;
    if heading.strip_prefix("diff --git ").unwrap_or(heading) != file_header {
        return None;
    }
    let header = section
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("@@"));
    let mut paths = (None, None);
    let mut objects = None;
    for line in header {
        if let Some(old) = line.strip_prefix("--- ") {
            paths.0 = Some(old);
        } else if let Some(new) = line.strip_prefix("+++ ") {
            paths.1 = Some(new);
        } else if let Some(ids) = line.strip_prefix("index ") {
            if ids != index_line {
                return None;
            }
            objects = ids
                .split_whitespace()
                .next()
                .and_then(|pair| pair.split_once(".."));
        }
    }
    let (old_id, new_id) = objects?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let outline_side = |marker, old, oid| -> Option<Vec<Symbol>> {
        if marker == "/dev/null" {
            return Some(Vec::new());
        }
        let path = patch_path(marker)?;
        let path = path
            .strip_prefix(if old { "a/" } else { "b/" })?
            .strip_prefix(prefix)?;
        if !valid_relative_path(path) {
            return None;
        }
        let Some(grammar) = Grammar::for_path(path) else {
            return Some(Vec::new());
        };
        let source = load_blob(workspace, oid, path, worktree && !old)?;
        patch_matches(&source, section, old)?;
        let result =
            syntax::outline(grammar, &source, 4_000, || Instant::now() >= deadline).ok()?;
        (!result.truncated).then_some(result.symbols)
    };
    let old = outline_side(paths.0?, true, old_id)?;
    let new = outline_side(paths.1?, false, new_id)?;
    Some(DiffSymbols { old, new })
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

fn load_blob(workspace: &Path, oid: &str, path: &str, worktree: bool) -> Option<String> {
    if !matches!(oid.len(), 40 | 64) || !super::valid_hash(oid) {
        return None;
    }
    let bytes = match run_git(workspace, &["cat-file", "-s", oid]) {
        Ok(size) => {
            if size.trim().parse::<usize>().ok()? > MAX_SOURCE_BYTES {
                return None;
            }
            run_git_bytes(workspace, &["cat-file", "blob", oid]).ok()?
        }
        Err(_) if worktree => {
            let root = Workspace::new(workspace.to_str()?).ok()?;
            let target = root.resolve(path).ok()?;
            let metadata = target.metadata().ok()?;
            if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES as u64 {
                return None;
            }
            let mut bytes = Vec::new();
            std::fs::File::open(target)
                .ok()?
                .take(MAX_SOURCE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > MAX_SOURCE_BYTES || hash_blob(workspace, &bytes)?.trim() != oid {
                return None;
            }
            bytes
        }
        Err(_) => return None,
    };
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn hash_blob(workspace: &Path, bytes: &[u8]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
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

    fn index_line(patch: &str) -> &str {
        patch
            .lines()
            .find_map(|line| line.strip_prefix("index "))
            .unwrap()
    }

    fn file_header(patch: &str) -> &str {
        patch
            .lines()
            .find_map(|line| line.strip_prefix("diff --git "))
            .unwrap()
    }

    #[test]
    fn resolves_worktree_index_and_commit_blobs_for_each_diff_side() {
        let root = temp_root("git-symbols");
        let git = |args: &[&str]| run_git(&root, args).unwrap();
        let save = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
        let changes = |path: &str, patch: &str| {
            for_diff(
                &root,
                Some(path),
                None,
                0,
                file_header(patch),
                index_line(patch),
            )
            .unwrap()
        };
        let commit = |parent: Option<&str>| {
            let tree = git(&["write-tree"]);
            let mut args = vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                tree.trim(),
                "-m",
                "fixture",
            ];
            if let Some(parent) = parent {
                args.extend(["-p", parent]);
            }
            let commit = git(&args).trim().to_string();
            git(&["update-ref", "HEAD", &commit]);
            commit
        };

        git(&["init", "-b", "main"]);
        let before = "fn old() {\n    let repeated_body = 1;\n    println!(\"{repeated_body}\");\n}\nfn stable() {}\n";
        let after = before.replace("fn old", "fn new");
        save("old.rs", before);
        git(&["add", "old.rs"]);
        let initial = commit(None);

        git(&["mv", "old.rs", "new.rs"]);
        save("new.rs", &after);
        let patch = diff(&root, "new.rs").unwrap();
        assert!(patch.contains("rename from old.rs"), "{patch}");
        let symbols = changes("new.rs", &patch).unwrap();
        assert_eq!(symbols.old[0].name, "old");
        assert_eq!(symbols.new[0].name, "new");
        save("new.rs", &after.replace("fn new", "fn changed_again"));
        assert!(changes("new.rs", &patch).is_none());

        save("new.rs", &after);
        git(&["add", "new.rs"]);
        let committed = commit(Some(&initial));
        let patch = show(&root, &committed).unwrap();
        save("new.rs", "fn different_worktree() {}\n");
        let symbols = for_diff(
            &root,
            None,
            Some(&committed),
            0,
            file_header(&patch),
            index_line(&patch),
        )
        .unwrap()
        .unwrap();
        assert_eq!(symbols.old[0].name, "old");
        assert_eq!(symbols.new[0].name, "new");

        std::fs::remove_file(root.join("new.rs")).unwrap();
        let patch = diff(&root, "new.rs").unwrap();
        let symbols = changes("new.rs", &patch).unwrap();
        assert_eq!(symbols.old[0].name, "new");
        assert!(symbols.new.is_empty());

        save("新 file.ts", "export function added() {}\n");
        let patch = diff(&root, "新 file.ts").unwrap();
        let symbols = changes("新 file.ts", &patch).unwrap();
        assert!(symbols.old.is_empty());
        assert_eq!(symbols.new[0].name, "added");
        git(&["add", "新 file.ts"]);
        std::fs::remove_file(root.join("新 file.ts")).unwrap();
        let patch = diff(&root, "新 file.ts").unwrap();
        let symbols = changes("新 file.ts", &patch).unwrap();
        assert_eq!(symbols.new[0].name, "added");

        for (path, source) in [
            ("worker.py", "def run():\n    return 1\n"),
            (
                "Worker.java",
                "class Worker {\n    int run() { return 1; }\n}\n",
            ),
        ] {
            save(path, source);
            let patch = diff(&root, path).unwrap();
            let symbols = changes(path, &patch).unwrap();
            assert!(symbols.old.is_empty(), "{path}");
            assert!(
                symbols.new.iter().any(|symbol| symbol.name == "run"),
                "{path}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
