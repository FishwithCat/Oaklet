//! Display-only file diffs for file-mutation tools.
//!
//! The model never sees this data. Tools attach context-trimmed before/after
//! hunks to their result so the chat UI can draw a diff card, and pending write
//! calls use the same shape for a preview while the tool runs.

use serde::Serialize;

/// Context lines kept on each side of an applied change.
const DIFF_CONTEXT: usize = 3;
/// Above this many matrix cells the middle of a file is treated as replaced
/// wholesale; the diff stays readable without an unbounded quadratic walk.
const MAX_LCS_CELLS: usize = 1_000_000;

/// One changed file fragment as the UI consumes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    /// `None` when there was no prior text (a new file or a pure insertion).
    pub old_text: Option<String>,
    pub new_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Derive one diff fragment per changed hunk.
///
/// A missing `before` means the call created the file: the whole content is an
/// addition. Identical text produces no card. CRLF is ignored so a line-ending
/// difference alone does not turn one edited line into a whole-file diff.
pub fn compute_diffs(path: &str, before: Option<&str>, after: &str) -> Vec<FileDiff> {
    let old_lines = content_lines(before.unwrap_or(""));
    let new_lines = content_lines(after);
    if before.is_some() && old_lines == new_lines {
        return Vec::new();
    }
    if before.is_none() && old_lines.is_empty() && new_lines.is_empty() {
        return vec![FileDiff {
            path: path.to_string(),
            old_text: None,
            new_text: String::new(),
        }];
    }

    let steps = diff_steps(&old_lines, &new_lines);
    hunk_ranges(&steps)
        .into_iter()
        .map(|(start, end)| {
            let mut old: Vec<&str> = Vec::new();
            let mut new: Vec<&str> = Vec::new();
            for step in &steps[start..end] {
                match *step {
                    Step::Equal(old_index, new_index) => {
                        old.push(old_lines[old_index]);
                        new.push(new_lines[new_index]);
                    }
                    Step::Delete(old_index) => old.push(old_lines[old_index]),
                    Step::Insert(new_index) => new.push(new_lines[new_index]),
                }
            }
            FileDiff {
                path: path.to_string(),
                old_text: (!old.is_empty()).then(|| old.join("\n")),
                new_text: new.join("\n"),
            }
        })
        .collect()
}

/// Content lines, applying the same trailing-newline rule the UI uses: one
/// final newline terminates the last line instead of adding an empty one.
fn content_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Line steps with a common-prefix/suffix trim and a bounded LCS in the middle.
fn diff_steps(old: &[&str], new: &[&str]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        steps.push(Step::Equal(prefix, prefix));
        prefix += 1;
    }

    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > prefix && new_end > prefix && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }

    let old_middle = &old[prefix..old_end];
    let new_middle = &new[prefix..new_end];
    if old_middle.is_empty() {
        for index in 0..new_middle.len() {
            steps.push(Step::Insert(prefix + index));
        }
    } else if new_middle.is_empty() {
        for index in 0..old_middle.len() {
            steps.push(Step::Delete(prefix + index));
        }
    } else if old_middle.len().saturating_mul(new_middle.len()) <= MAX_LCS_CELLS {
        lcs_steps(old_middle, new_middle, prefix, &mut steps);
    } else {
        for index in 0..old_middle.len() {
            steps.push(Step::Delete(prefix + index));
        }
        for index in 0..new_middle.len() {
            steps.push(Step::Insert(prefix + index));
        }
    }

    for offset in 0..old.len() - old_end {
        steps.push(Step::Equal(old_end + offset, new_end + offset));
    }
    steps
}

fn lcs_steps(old: &[&str], new: &[&str], offset: usize, steps: &mut Vec<Step>) {
    let width = new.len() + 1;
    let mut table = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i * width + j] = if old[i] == new[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }

    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            steps.push(Step::Equal(offset + i, offset + j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            steps.push(Step::Delete(offset + i));
            i += 1;
        } else {
            steps.push(Step::Insert(offset + j));
            j += 1;
        }
    }
    while i < old.len() {
        steps.push(Step::Delete(offset + i));
        i += 1;
    }
    while j < new.len() {
        steps.push(Step::Insert(offset + j));
        j += 1;
    }
}

/// Step ranges with context, merging changes separated by at most twice the
/// context so nearby edits share one fragment.
fn hunk_ranges(steps: &[Step]) -> Vec<(usize, usize)> {
    let mut changes = Vec::new();
    let mut index = 0;
    while index < steps.len() {
        if matches!(steps[index], Step::Equal(..)) {
            index += 1;
            continue;
        }
        let start = index;
        while index < steps.len() && !matches!(steps[index], Step::Equal(..)) {
            index += 1;
        }
        changes.push((start, index));
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (start, end) in changes {
        let start = start.saturating_sub(DIFF_CONTEXT);
        let end = (end + DIFF_CONTEXT).min(steps.len());
        if let Some(last) = ranges.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        ranges.push((start, end));
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::{FileDiff, compute_diffs};

    fn diff(path: &str, before: Option<&str>, after: &str) -> Vec<FileDiff> {
        compute_diffs(path, before, after)
    }

    #[test]
    fn creates_show_every_line_as_an_addition() {
        assert_eq!(
            diff("a.txt", None, "one\ntwo\n"),
            vec![FileDiff {
                path: "a.txt".into(),
                old_text: None,
                new_text: "one\ntwo".into(),
            }]
        );
    }

    #[test]
    fn keeps_three_context_lines_and_merges_nearby_changes() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        let after = "a\nb\nc\nD\ne\nf\ng\nh\ni\nJ\nk\n";
        let diffs = diff("a.txt", Some(before), after);
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            diffs[0].old_text.as_deref(),
            Some("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk")
        );
        assert_eq!(diffs[0].new_text, "a\nb\nc\nD\ne\nf\ng\nh\ni\nJ\nk");
    }

    #[test]
    fn splits_distant_changes_into_separate_fragments() {
        let before = (1..=40)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let after = before
            .replacen("\n2\n", "\nTWO\n", 1)
            .replacen("\n35\n", "\nTHIRTY-FIVE\n", 1);
        let diffs = diff("a.txt", Some(&before), &after);
        assert_eq!(diffs.len(), 2);
        assert!(diffs[0].new_text.contains("TWO"));
        assert!(diffs[1].new_text.contains("THIRTY-FIVE"));
        assert!(!diffs[0].new_text.contains("THIRTY-FIVE"));
    }

    #[test]
    fn deletions_have_no_new_text() {
        let diffs = diff("a.txt", Some("one\ntwo\n"), "");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].old_text.as_deref(), Some("one\ntwo"));
        assert_eq!(diffs[0].new_text, "");
    }

    #[test]
    fn identical_text_and_line_endings_only_yield_no_diff() {
        assert!(diff("a.txt", Some("one\n"), "one\n").is_empty());
        assert!(diff("a.txt", Some("one\r\ntwo\r\n"), "one\ntwo\n").is_empty());
    }
}
