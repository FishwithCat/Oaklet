//! Recoverable spill storage for oversized model-facing text.
//!
//! Tool output and pasted attachments are written under
//! `<workspace>/.oaklet/spill/<run_id>/` before their inline copy is trimmed,
//! so the model can read the full text back with `read_file`. Every failure
//! returns `None` and leaves callers on their previous behavior: dropping the
//! only copy is worse than an over-long message.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Longest sanitized path segment; well under common 255-byte limits.
const MAX_SEGMENT_CHARS: usize = 96;

/// A file written by the spill writer.
#[derive(Clone, Debug)]
pub(crate) struct Spilled {
    /// Workspace-relative location, as shown to the model.
    pub relative: PathBuf,
    /// Line count of the complete text.
    pub lines: usize,
    /// UTF-8 byte length of the complete text.
    pub bytes: usize,
}

/// Write `content` to `<root>/.oaklet/spill/<run_id>/<suggested>.txt`.
///
/// The file is created exclusively with owner-only permissions, so a planted
/// symlink cannot redirect the write. Name collisions get a numeric suffix.
pub(crate) fn save(root: &Path, run_id: &str, suggested: &str, content: &str) -> Option<Spilled> {
    let base = Path::new(".oaklet").join("spill");
    let directory = base.join(segment(run_id));
    let absolute_base = root.join(&base);
    let absolute_dir = root.join(&directory);
    // Reject a symlinked `.oaklet` or `spill` before creating anything below it.
    if !inside_workspace(root, &absolute_base) {
        return None;
    }
    fs::create_dir_all(&absolute_dir).ok()?;
    if !absolute_dir.canonicalize().ok()?.starts_with(root) {
        return None;
    }
    ignore_in_git(&absolute_base);

    for suffix in 0..100 {
        let stem = match suffix {
            0 => segment(suggested),
            _ => format!("{}-{suffix}", segment(suggested)),
        };
        let relative = directory.join(format!("{stem}.txt"));
        let path = root.join(&relative);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                if file.write_all(content.as_bytes()).is_err() {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return None;
                }
                return Some(Spilled {
                    relative,
                    lines: content.lines().count(),
                    bytes: content.len(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Whether the nearest existing ancestor of `target` resolves inside `root`.
/// This catches a symlinked `.oaklet` before `create_dir_all` follows it.
fn inside_workspace(root: &Path, target: &Path) -> bool {
    let mut ancestor = target;
    while ancestor.symlink_metadata().is_err() {
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return false,
        }
    }
    ancestor
        .canonicalize()
        .map(|real| real.starts_with(root))
        .unwrap_or(false)
}

/// Keep spill artifacts out of the user's `git status`, best effort.
fn ignore_in_git(directory: &Path) {
    let path = directory.join(".gitignore");
    if path.exists() {
        return;
    }
    let _ = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(b"*\n"));
}

/// One filesystem-safe path segment: no separators, no `.`/`..`, bounded.
fn segment(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .take(MAX_SEGMENT_CHARS)
        .collect();
    let trimmed = mapped.trim_matches('.');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Bounded head and tail excerpt of a text.
#[derive(Debug)]
pub(crate) struct Preview {
    pub head: String,
    pub tail: String,
    /// Whole lines between the two excerpts.
    pub omitted_lines: usize,
    /// Whether a character budget cut inside an excerpt.
    pub clipped: bool,
}

/// Keep the first `head_lines` and last `tail_lines`, then cap each end by
/// characters so one very long line cannot blow the inline budget.
pub(crate) fn preview(
    text: &str,
    head_lines: usize,
    tail_lines: usize,
    head_chars: usize,
    tail_chars: usize,
) -> Preview {
    let lines: Vec<&str> = text.lines().collect();
    let head_count = head_lines.min(lines.len());
    let tail_count = tail_lines.min(lines.len() - head_count);
    let head_source = lines[..head_count].join("\n");
    let tail_source = lines[lines.len() - tail_count..].join("\n");
    let (head, head_clipped) = clip_head(&head_source, head_chars);
    let (mut tail, mut tail_clipped) = clip_tail(&tail_source, tail_chars);
    // Fewer lines than the head budget leaves no line for the tail. If the head
    // was then clipped by characters, take the tail from the remainder so the
    // end of the file still appears in the preview.
    if tail.is_empty() && head_clipped {
        let head_len = head.chars().count();
        let remainder: String = head_source.chars().skip(head_len).collect();
        (tail, tail_clipped) = clip_tail(&remainder, tail_chars);
    }
    Preview {
        head,
        tail,
        omitted_lines: lines.len() - head_count - tail_count,
        clipped: head_clipped || tail_clipped,
    }
}

fn clip_head(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

fn clip_tail(text: &str, max_chars: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().skip(total - max_chars).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::{preview, save, segment};
    use crate::tools::test_support::temp_root;

    #[test]
    fn sanitizes_path_segments() {
        assert_eq!(segment("call_1-a.b"), "call_1-a.b");
        assert_eq!(segment("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(segment(".."), "x");
        assert_eq!(segment(""), "x");
        assert_eq!(segment("a/b\\c"), "a_b_c");
        assert!(segment(&"x".repeat(500)).len() <= 96);
    }

    #[test]
    fn saves_under_the_run_directory_and_reports_size() {
        let root = temp_root("spill-save");
        let text = "first\nsecond\nthird";
        let spilled = save(&root, "run-1", "run_command-call_9", text).unwrap();

        assert_eq!(
            spilled.relative,
            std::path::Path::new(".oaklet/spill/run-1/run_command-call_9.txt")
        );
        assert_eq!(spilled.lines, 3);
        assert_eq!(spilled.bytes, text.len());
        assert_eq!(
            std::fs::read_to_string(root.join(&spilled.relative)).unwrap(),
            text
        );

        // A second save with the same name does not overwrite the first.
        let second = save(&root, "run-1", "run_command-call_9", "other").unwrap();
        assert_ne!(second.relative, spilled.relative);
        assert_eq!(
            std::fs::read_to_string(root.join(&spilled.relative)).unwrap(),
            text
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_a_spill_directory_that_escapes_the_workspace() {
        let root = temp_root("spill-escape");
        let outside = temp_root("spill-outside");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join(".oaklet")).unwrap();

        #[cfg(unix)]
        assert!(save(&root, "run-1", "output", "x").is_none());
        #[cfg(unix)]
        assert!(!outside.join("spill").exists());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn previews_the_first_and_last_lines() {
        let text = (1..=200)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = preview(&text, 3, 2, 1_000, 1_000);

        assert_eq!(preview.head, "line 1\nline 2\nline 3");
        assert_eq!(preview.tail, "line 199\nline 200");
        assert_eq!(preview.omitted_lines, 195);
        assert!(!preview.clipped);
    }

    #[test]
    fn preview_caps_long_lines_by_character() {
        let text = format!("{}\n{}", "h".repeat(50), "t".repeat(50));
        let preview = preview(&text, 1, 1, 10, 5);

        assert_eq!(preview.head, "h".repeat(10));
        assert_eq!(preview.tail, "t".repeat(5));
        assert_eq!(preview.omitted_lines, 0);
        assert!(preview.clipped);
    }

    #[test]
    fn preview_keeps_the_tail_of_a_few_long_lines() {
        let text = format!("{}\n... error: connection reset", "h".repeat(20_000));
        let preview = preview(&text, 80, 40, 6_000, 2_000);

        assert_eq!(preview.head, "h".repeat(6_000));
        assert!(preview.tail.ends_with("... error: connection reset"));
        assert_eq!(preview.omitted_lines, 0);
        assert!(preview.clipped);
    }
}
