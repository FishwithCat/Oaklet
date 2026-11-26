//! Hunk matching against file text, with Codex's tolerance passes.

use super::parse::PatchHunk;

#[derive(Debug)]
pub(super) struct HunkFailure {
    pub(super) line: usize,
    pub(super) stage: &'static str,
    pub(super) preview: String,
}

pub(super) fn mismatch_message(display: &str, failure: &HunkFailure) -> String {
    format!(
        "cannot patch \"{display}\": hunk at patch line {} ({}) does not match — {}",
        failure.line, failure.stage, failure.preview
    )
}

struct Replacement {
    start: usize,
    old: usize,
    new: Vec<String>,
}

/// Apply parsed hunks to LF text, returning no partial result on a mismatch.
pub(super) fn apply_hunks(before: &str, hunks: &[PatchHunk]) -> Result<String, HunkFailure> {
    let mut original: Vec<String> = before.split('\n').map(str::to_string).collect();
    if original.last().is_some_and(String::is_empty) {
        original.pop();
    }

    let mut replacements: Vec<Replacement> = Vec::new();
    let mut cursor = 0;
    for hunk in hunks {
        if let Some(context) = &hunk.context {
            let pattern = [context.clone()];
            match seek(&original, &pattern, cursor, false) {
                Some(found) => cursor = found + 1,
                None => {
                    return Err(HunkFailure {
                        line: hunk.line,
                        stage: "context",
                        preview: preview(&pattern),
                    });
                }
            }
        }

        if hunk.old.is_empty() {
            let start = if original.last().is_some_and(String::is_empty) {
                original.len().saturating_sub(1)
            } else {
                original.len()
            };
            replacements.push(Replacement {
                start,
                old: 0,
                new: hunk.new.clone(),
            });
            continue;
        }

        let mut pattern = hunk.old.clone();
        let mut new_lines = hunk.new.clone();
        let mut found = seek(&original, &pattern, cursor, hunk.eof);
        // Codex retries without a trailing empty pattern line, which appears
        // when the patch itself ends with a newline.
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern.pop();
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines.pop();
            }
            found = seek(&original, &pattern, cursor, hunk.eof);
        }
        let Some(start) = found else {
            return Err(HunkFailure {
                line: hunk.line,
                stage: "old lines",
                preview: preview(&hunk.old),
            });
        };
        cursor = start + pattern.len();
        replacements.push(Replacement {
            start,
            old: pattern.len(),
            new: new_lines,
        });
    }

    replacements.sort_by_key(|replacement| replacement.start);
    let mut lines = original;
    for replacement in replacements.iter().rev() {
        let tail = lines.split_off(replacement.start + replacement.old);
        lines.truncate(replacement.start);
        lines.extend(replacement.new.iter().cloned());
        lines.extend(tail);
    }
    if !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// Codex `seek_sequence`: exact, trim-end, trim-both, then punctuation-normalized.
fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pattern.len();
    let first = if eof { last } else { start };
    if first > last {
        return None;
    }
    for pass in 0..4 {
        for index in first..=last {
            if pattern
                .iter()
                .enumerate()
                .all(|(offset, expected)| line_matches(expected, &lines[index + offset], pass))
            {
                return Some(index);
            }
        }
    }
    None
}

fn line_matches(expected: &str, actual: &str, pass: usize) -> bool {
    match pass {
        0 => expected == actual,
        1 => expected.trim_end() == actual.trim_end(),
        2 => expected.trim() == actual.trim(),
        _ => normalize_punctuation(expected) == normalize_punctuation(actual),
    }
}

fn normalize_punctuation(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}'..='\u{201B}' => '\'',
            '\u{201C}'..='\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn preview(lines: &[String]) -> String {
    let first = lines.first().map(String::as_str).unwrap_or_default();
    let mut characters = first.chars();
    let head: String = characters.by_ref().take(80).collect();
    if characters.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

pub(super) fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub(super) fn uses_crlf(text: &str) -> bool {
    let bytes = text.as_bytes();
    if !bytes.contains(&b'\n') {
        return false;
    }
    !bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r'))
}

pub(super) fn restore_line_endings(crlf: bool, patched: String) -> String {
    if crlf {
        patched.replace('\n', "\r\n")
    } else {
        patched
    }
}
