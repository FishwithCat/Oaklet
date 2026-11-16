//! Recoverable spill storage for oversized model-facing text.
//!
//! Tool output and pasted attachments are written under
//! `<app data>/spill/session-<hash>/` before their inline copy is trimmed, so
//! the model can read the full text back with `read_file`. The root lives
//! outside the workspace: spill files are an app artifact, not project files.
//! Every failure returns `None` and leaves callers on their previous behavior:
//! dropping the only copy is worse than an over-long message.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Longest sanitized path segment; well under common 255-byte limits.
const MAX_SEGMENT_CHARS: usize = 96;

/// Spill files older than this are removed by the startup sweep.
pub(crate) const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A file written by the spill writer.
#[derive(Clone, Debug)]
pub(crate) struct Spilled {
    /// Absolute location, as shown to the model.
    pub path: PathBuf,
    /// Line count of the complete text.
    pub lines: usize,
    /// UTF-8 byte length of the complete text.
    pub bytes: usize,
}

/// Write `content` to `<root>/session-<hash>/<suggested>.txt`.
///
/// The file is created exclusively with owner-only permissions, so a planted
/// symlink cannot redirect the write. Name collisions get a numeric suffix.
pub(crate) fn save(
    root: &Path,
    session_id: &str,
    suggested: &str,
    content: &str,
) -> Option<Spilled> {
    fs::create_dir_all(root).ok()?;
    let directory = root.join(session_directory(session_id));
    fs::create_dir_all(&directory).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
    }
    // Reject a planted session-directory symlink before writing through it.
    let canonical_root = root.canonicalize().ok()?;
    if !directory.canonicalize().ok()?.starts_with(&canonical_root) {
        return None;
    }

    for suffix in 0..100 {
        let stem = match suffix {
            0 => segment(suggested),
            _ => format!("{}-{suffix}", segment(suggested)),
        };
        let path = directory.join(format!("{stem}.txt"));
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
                    path,
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

/// Delete spill files older than `retention` and prune empty session
/// directories. Best effort: runs once at startup, before any run can write.
pub(crate) fn cleanup(root: &Path, retention: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(is_session_directory) {
            continue;
        }
        // `file_type` does not follow symlinks; a planted `session-*` link is
        // left untouched instead of being descended into.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(entry.path()) else {
            continue;
        };
        let mut empty = true;
        for file in files.flatten() {
            if !file.file_type().is_ok_and(|kind| kind.is_file()) {
                empty = false;
                continue;
            }
            let Ok(modified) = file.metadata().and_then(|metadata| metadata.modified()) else {
                empty = false;
                continue;
            };
            // A future timestamp is not older than the cutoff; keep it.
            if now.duration_since(modified).unwrap_or_default() < retention {
                empty = false;
                continue;
            }
            if fs::remove_file(file.path()).is_err() {
                empty = false;
            }
        }
        if empty {
            let _ = fs::remove_dir(entry.path());
        }
    }
}

/// Stable session directory name. FNV-1a keeps it identical across processes
/// and releases without adding a hashing dependency.
fn session_directory(session_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in session_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("session-{hash:016x}")
}

/// Whether `name` is one of this module's session directories.
fn is_session_directory(name: &str) -> bool {
    name.strip_prefix("session-")
        .is_some_and(|hash| hash.len() == 16 && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
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
    use super::{RETENTION, cleanup, preview, save, segment, session_directory};
    use crate::tools::test_support::temp_root;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

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
    fn saves_under_the_session_directory_and_reports_size() {
        let root = temp_root("spill-save");
        let text = "first\nsecond\nthird";
        let spilled = save(&root, "run-1", "run_command-call_9", text).unwrap();

        assert!(
            spilled
                .path
                .starts_with(root.join(session_directory("run-1")))
        );
        assert_eq!(spilled.path.file_name().unwrap(), "run_command-call_9.txt");
        assert_eq!(spilled.lines, 3);
        assert_eq!(spilled.bytes, text.len());
        assert_eq!(std::fs::read_to_string(&spilled.path).unwrap(), text);

        // A second save with the same name does not overwrite the first.
        let second = save(&root, "run-1", "run_command-call_9", "other").unwrap();
        assert_ne!(second.path, spilled.path);
        assert_eq!(std::fs::read_to_string(&spilled.path).unwrap(), text);

        // The app's spill root and files are private to the current user.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&spilled.path), 0o600);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn keeps_session_directories_stable_across_saves() {
        let root = temp_root("spill-session");
        let first = save(&root, "run-1", "a", "x").unwrap();
        let second = save(&root, "run-1", "b", "y").unwrap();
        assert_eq!(first.path.parent(), second.path.parent());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_a_session_directory_that_escapes_the_root() {
        let root = temp_root("spill-escape");
        let outside = temp_root("spill-outside");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join(session_directory("run-1"))).unwrap();

        #[cfg(unix)]
        assert!(save(&root, "run-1", "output", "x").is_none());
        #[cfg(unix)]
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn cleanup_expires_old_files_and_prunes_their_directory() {
        let root = temp_root("spill-cleanup");
        let old = save(&root, "old-session", "a", "old").unwrap();
        let fresh = save(&root, "fresh-session", "b", "fresh").unwrap();
        age(&old.path, 40);
        cleanup(&root, RETENTION);

        assert!(!old.path.exists());
        assert!(!old.path.parent().unwrap().exists());
        assert!(fresh.path.exists());
        assert!(fresh.path.parent().unwrap().exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cleanup_leaves_symlinked_session_directories_alone() {
        let root = temp_root("spill-cleanup-link");
        let outside = temp_root("spill-cleanup-outside");
        let victim = outside.join("keep.txt");
        std::fs::write(&victim, "keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join(session_directory("evil"))).unwrap();

        cleanup(&root, RETENTION);

        #[cfg(unix)]
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    fn age(path: &Path, days: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60))
            .unwrap();
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
