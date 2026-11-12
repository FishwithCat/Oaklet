//! Codex-style `apply_patch` tool.
//!
//! One patch can add, update, delete, or move several files. Every operation is
//! parsed and checked against an in-memory simulation before anything is
//! written; once committing starts, operations run in order and stop at the
//! first failure or cancellation without rollback.
//!
//! The syntax and matcher follow deepseek-harness `fs-patch`, which adapts the
//! OpenAI Codex parser and `seek_sequence` behavior (Apache-2.0).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::fs::{MAX_FILE_BYTES, write_atomic};
use super::{Cancel, Tool, ToolContext, ToolError, ToolOutput, parse_args};

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const EOF: &str = "*** End of File";
const MOVE: &str = "*** Move to: ";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";

type ParseError = (usize, String);

#[derive(Clone, Debug, PartialEq, Eq)]
struct PatchHunk {
    /// One-based input line containing the marker or first implicit hunk line.
    line: usize,
    /// Text after `@@ `, used as a search anchor.
    context: Option<String>,
    /// Context and removed lines without prefixes.
    old: Vec<String>,
    /// Context and added lines without prefixes.
    new: Vec<String>,
    /// Whether `*** End of File` follows this hunk.
    eof: bool,
}

impl PatchHunk {
    fn new(line: usize) -> Self {
        Self {
            line,
            context: None,
            old: Vec::new(),
            new: Vec::new(),
            eof: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.old.is_empty() && self.new.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PatchOperation {
    Add {
        path: String,
        line: usize,
        content: String,
    },
    Delete {
        path: String,
        line: usize,
    },
    Update {
        path: String,
        line: usize,
        move_to: Option<String>,
        hunks: Vec<PatchHunk>,
    },
}

impl PatchOperation {
    fn path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Delete { path, .. } | Self::Update { path, .. } => path,
        }
    }
}

/// Parse a complete patch without touching the filesystem.
fn parse_patch(input: &str) -> Result<Vec<PatchOperation>, ParseError> {
    let leading = input.len() - input.trim_start().len();
    let offset = input[..leading].matches('\n').count();
    let mut lines: Vec<(usize, String)> = input
        .trim()
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            (
                offset + index + 1,
                line.strip_suffix('\r').unwrap_or(line).to_string(),
            )
        })
        .collect();

    // Codex accepts a literal shell heredoc wrapper; the contents are never
    // evaluated, so the wrapper is simply discarded.
    if lines.len() >= 4 {
        let first = lines[0].1.trim();
        let last = lines[lines.len() - 1].1.trim();
        if matches!(first, "<<EOF" | "<<'EOF'" | "<<\"EOF\"") && last.ends_with("EOF") {
            lines = lines[1..lines.len() - 1].to_vec();
        }
    }

    let (start_line, start) = lines.first().ok_or((1, expected(BEGIN)))?;
    if start.trim() != BEGIN {
        return Err((*start_line, expected(BEGIN)));
    }
    let (end_line, end) = lines.last().unwrap();
    if end.trim() != END {
        return Err((*end_line, expected(END)));
    }

    let mut operations: Vec<PatchOperation> = Vec::new();
    let mut ended = false;
    let count = lines.len();

    for (index, (number, text)) in lines.iter().enumerate().skip(1) {
        let is_last = index == count - 1;
        // Inside an update, indentation makes a marker ordinary context; the
        // final End marker is still recognized so a missing close is caught.
        let in_update = matches!(operations.last(), Some(PatchOperation::Update { .. }));
        let header = if in_update && !is_last {
            text.trim_end()
        } else {
            text.trim()
        };

        if ended && !is_last {
            if !header.is_empty() {
                return Err((*number, format!("Unexpected content after '{END}'")));
            }
            continue;
        }

        if header == END {
            ensure_update(&operations, *number)?;
            ended = true;
        } else if let Some(path) = header.strip_prefix(ADD) {
            ensure_update(&operations, *number)?;
            operations.push(PatchOperation::Add {
                path: path.to_string(),
                line: *number,
                content: String::new(),
            });
        } else if let Some(path) = header.strip_prefix(DELETE) {
            ensure_update(&operations, *number)?;
            operations.push(PatchOperation::Delete {
                path: path.to_string(),
                line: *number,
            });
        } else if let Some(path) = header.strip_prefix(UPDATE) {
            ensure_update(&operations, *number)?;
            operations.push(PatchOperation::Update {
                path: path.to_string(),
                line: *number,
                move_to: None,
                hunks: Vec::new(),
            });
        } else if operations.is_empty() && header.starts_with("*** Environment ID:") {
            return Err((
                *number,
                "Environment ID headers are unsupported; paths are resolved against the workspace"
                    .into(),
            ));
        } else if let Some(PatchOperation::Add { content, .. }) = operations.last_mut() {
            if let Some(added) = text.strip_prefix('+') {
                content.push_str(added);
                content.push('\n');
            } else {
                return Err((
                    *number,
                    "Expected an Add File, Delete File, or Update File header; added content must start with +"
                        .into(),
                ));
            }
        } else if matches!(operations.last(), Some(PatchOperation::Update { .. })) {
            read_update(operations.last_mut().unwrap(), text, *number)?;
        } else {
            return Err((
                *number,
                "Expected an Add File, Delete File, or Update File header; added content must start with +"
                    .into(),
            ));
        }
    }

    Ok(operations)
}

fn expected(marker: &str) -> String {
    format!("Expected '{marker}'")
}

fn ensure_update(operations: &[PatchOperation], line: usize) -> Result<(), ParseError> {
    let Some(PatchOperation::Update {
        path,
        line: operation_line,
        hunks,
        ..
    }) = operations.last()
    else {
        return Ok(());
    };
    if hunks.is_empty() {
        return Err((
            *operation_line,
            format!("Update for '{path}' requires at least one hunk"),
        ));
    }
    if hunks.last().is_some_and(PatchHunk::is_empty) {
        return Err((
            line,
            "Update hunk must contain context, added, or removed lines".into(),
        ));
    }
    Ok(())
}

fn read_update(
    operation: &mut PatchOperation,
    line: &str,
    number: usize,
) -> Result<(), ParseError> {
    let PatchOperation::Update { hunks, move_to, .. } = operation else {
        return Ok(());
    };
    let text = line.trim_end();
    let marker = text == "@@" || text.starts_with("@@ ");

    if hunks.last().is_some_and(|hunk| hunk.eof) {
        if text.is_empty() {
            return Ok(());
        }
        if !marker {
            return Err((number, "Expected '@@' after '*** End of File'".into()));
        }
    }

    if hunks.is_empty()
        && move_to.is_none()
        && let Some(path) = text.strip_prefix(MOVE)
    {
        *move_to = Some(path.to_string());
        return Ok(());
    }

    if marker {
        if let Some(last) = hunks.last()
            && last.is_empty()
        {
            return Err((
                number,
                "Update hunk must contain context, added, or removed lines".into(),
            ));
        }
        let mut hunk = PatchHunk::new(number);
        if let Some(context) = text.strip_prefix("@@ ") {
            hunk.context = Some(context.to_string());
        }
        hunks.push(hunk);
        return Ok(());
    }

    if text == EOF {
        if let Some(last) = hunks.last_mut() {
            if last.is_empty() {
                return Err((
                    number,
                    "Update hunk must contain context, added, or removed lines".into(),
                ));
            }
            last.eof = true;
        }
        return Ok(());
    }

    let prefix = line.chars().next();
    if let Some(character) = prefix
        && !matches!(character, ' ' | '+' | '-')
    {
        return Err((
            number,
            "Expected a context (' '), added ('+'), removed ('-') line, or '@@'".into(),
        ));
    }
    if hunks.is_empty() {
        hunks.push(PatchHunk::new(number));
    }
    let content = line.get(1..).unwrap_or_default().to_string();
    let hunk = hunks.last_mut().unwrap();
    match prefix {
        Some('+') => hunk.new.push(content),
        Some('-') => hunk.old.push(content),
        _ => {
            hunk.old.push(content.clone());
            hunk.new.push(content);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct HunkFailure {
    line: usize,
    stage: &'static str,
    preview: String,
}

struct Replacement {
    start: usize,
    old: usize,
    new: Vec<String>,
}

/// Apply parsed hunks to LF text, returning no partial result on a mismatch.
fn apply_hunks(before: &str, hunks: &[PatchHunk]) -> Result<String, HunkFailure> {
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

fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Add,
    Update,
    Delete,
    Move,
}

struct ResolvedPatch {
    index: usize,
    kind: Kind,
    operation: PatchOperation,
    source: PathBuf,
    source_display: String,
    destination: Option<(PathBuf, String)>,
}

impl ResolvedPatch {
    fn line(&self) -> usize {
        match &self.operation {
            PatchOperation::Add { line, .. }
            | PatchOperation::Delete { line, .. }
            | PatchOperation::Update { line, .. } => *line,
        }
    }

    fn destination_display(&self) -> &str {
        self.destination
            .as_ref()
            .map(|(_, display)| display.as_str())
            .unwrap_or(&self.source_display)
    }

    /// `update path, patch line 2`, with move endpoints when applicable.
    fn describe(&self) -> String {
        let kind = match self.kind {
            Kind::Add => "add",
            Kind::Update => "update",
            Kind::Delete => "delete",
            Kind::Move => "move",
        };
        match self.kind {
            Kind::Move => format!(
                "{kind} {} -> {}, patch line {}",
                self.source_display,
                self.destination_display(),
                self.line()
            ),
            _ => format!("{kind} {}, patch line {}", self.source_display, self.line()),
        }
    }

    fn describe_short(&self) -> String {
        match self.kind {
            Kind::Add => format!("add {}", self.source_display),
            Kind::Update => format!("update {}", self.source_display),
            Kind::Delete => format!("delete {}", self.source_display),
            Kind::Move => format!(
                "move {} -> {}",
                self.source_display,
                self.destination_display()
            ),
        }
    }

    fn completed_line(&self) -> String {
        match self.kind {
            Kind::Add => format!("A {}", self.source_display),
            Kind::Update => format!("M {}", self.source_display),
            Kind::Delete => format!("D {}", self.source_display),
            Kind::Move => format!("M {}", self.destination_display()),
        }
    }
}

fn plan(
    ctx: &ToolContext,
    operations: Vec<PatchOperation>,
) -> Result<Vec<ResolvedPatch>, ToolError> {
    let mut plan = Vec::with_capacity(operations.len());
    for (index, operation) in operations.into_iter().enumerate() {
        if let Some(message) = symlink_source(ctx, &operation) {
            return Err(ToolError::Failed(message));
        }
        let source = ctx
            .workspace
            .resolve(operation.path())
            .map_err(ToolError::Failed)?;
        ctx.workspace
            .ensure_writable(&source)
            .map_err(ToolError::Failed)?;
        let source_display = ctx.workspace.relative(&source).display().to_string();

        let (kind, destination) = match &operation {
            PatchOperation::Add { .. } => (Kind::Add, None),
            PatchOperation::Delete { .. } => (Kind::Delete, None),
            PatchOperation::Update { move_to, .. } => match move_to {
                Some(path) => {
                    let destination = ctx.workspace.resolve(path).map_err(ToolError::Failed)?;
                    ctx.workspace
                        .ensure_writable(&destination)
                        .map_err(ToolError::Failed)?;
                    let display = ctx.workspace.relative(&destination).display().to_string();
                    (Kind::Move, Some((destination, display)))
                }
                None => (Kind::Update, None),
            },
        };

        plan.push(ResolvedPatch {
            index,
            kind,
            operation,
            source,
            source_display,
            destination,
        });
    }
    Ok(plan)
}

/// `resolve` canonicalizes symlinks, so deleting or moving one would silently
/// operate on its target instead. Refuse the ambiguous spelling.
fn symlink_source(ctx: &ToolContext, operation: &PatchOperation) -> Option<String> {
    let (verb, path) = match operation {
        PatchOperation::Delete { path, .. } => ("delete", path),
        PatchOperation::Update {
            path,
            move_to: Some(_),
            ..
        } => ("move", path),
        _ => return None,
    };
    let raw = Path::new(path.trim());
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        ctx.workspace.root().join(raw)
    };
    std::fs::symlink_metadata(&candidate)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        .then(|| {
            format!(
                "cannot {verb} \"{path}\": path is a symbolic link — operate on its target path instead"
            )
        })
}

#[derive(Clone)]
enum Simulated {
    File(String),
    Missing,
    Directory,
}

struct Preflight {
    states: HashMap<PathBuf, Simulated>,
}

impl Preflight {
    fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    fn state(&mut self, path: &Path, display: &str) -> Result<Simulated, String> {
        if let Some(state) = self.states.get(path) {
            return Ok(state.clone());
        }
        let state = match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => Simulated::Directory,
            Ok(metadata) => {
                if metadata.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "cannot read \"{display}\": file is too large ({} bytes)",
                        metadata.len()
                    ));
                }
                let text = std::fs::read_to_string(path)
                    .map_err(|error| format!("cannot read \"{display}\" as UTF-8 text: {error}"))?;
                if text.contains('\0') {
                    return Err(format!("cannot patch \"{display}\": binary file"));
                }
                Simulated::File(normalize_lf(&text))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Simulated::Missing,
            Err(error) => return Err(format!("cannot read \"{display}\": {error}")),
        };
        self.states.insert(path.to_path_buf(), state.clone());
        Ok(state)
    }

    fn check(&mut self, operation: &ResolvedPatch) -> Result<(), String> {
        match &operation.operation {
            PatchOperation::Add { content, .. } => {
                match self.state(&operation.source, &operation.source_display)? {
                    Simulated::Missing => {
                        self.states
                            .insert(operation.source.clone(), Simulated::File(content.clone()));
                        Ok(())
                    }
                    Simulated::Directory => Err(format!(
                        "cannot add \"{}\": not a regular file",
                        operation.source_display
                    )),
                    Simulated::File(_) => Err(format!(
                        "cannot add \"{}\": file already exists",
                        operation.source_display
                    )),
                }
            }
            PatchOperation::Delete { .. } => {
                match self.state(&operation.source, &operation.source_display)? {
                    Simulated::File(_) => {
                        self.states
                            .insert(operation.source.clone(), Simulated::Missing);
                        Ok(())
                    }
                    Simulated::Missing => Err(format!(
                        "cannot delete \"{}\": file not found",
                        operation.source_display
                    )),
                    Simulated::Directory => Err(format!(
                        "cannot delete \"{}\": not a regular file",
                        operation.source_display
                    )),
                }
            }
            PatchOperation::Update {
                hunks,
                move_to: None,
                ..
            } => {
                let before = match self.state(&operation.source, &operation.source_display)? {
                    Simulated::File(text) => text,
                    Simulated::Missing => {
                        return Err(format!(
                            "cannot update \"{}\": file not found",
                            operation.source_display
                        ));
                    }
                    Simulated::Directory => {
                        return Err(format!(
                            "cannot update \"{}\": not a regular file",
                            operation.source_display
                        ));
                    }
                };
                let after = apply_hunks(&before, hunks)
                    .map_err(|failure| mismatch_message(&operation.source_display, &failure))?;
                self.states
                    .insert(operation.source.clone(), Simulated::File(after));
                Ok(())
            }
            PatchOperation::Update {
                hunks,
                move_to: Some(_),
                ..
            } => {
                let Some((destination, destination_display)) = &operation.destination else {
                    return Err("Move destination is missing.".into());
                };
                if destination == &operation.source {
                    return Err(format!(
                        "cannot move \"{}\" to itself",
                        operation.source_display
                    ));
                }
                let before = match self.state(&operation.source, &operation.source_display)? {
                    Simulated::File(text) => text,
                    Simulated::Missing => {
                        return Err(format!(
                            "cannot move \"{}\": file not found",
                            operation.source_display
                        ));
                    }
                    Simulated::Directory => {
                        return Err(format!(
                            "cannot move \"{}\": not a regular file",
                            operation.source_display
                        ));
                    }
                };
                match self.state(destination, destination_display)? {
                    Simulated::Missing => {}
                    Simulated::File(_) => {
                        return Err(format!(
                            "cannot move \"{}\" to \"{destination_display}\": destination already exists",
                            operation.source_display
                        ));
                    }
                    Simulated::Directory => {
                        return Err(format!(
                            "cannot move \"{}\" to \"{destination_display}\": destination is a directory",
                            operation.source_display
                        ));
                    }
                }
                let after = apply_hunks(&before, hunks)
                    .map_err(|failure| mismatch_message(&operation.source_display, &failure))?;
                self.states
                    .insert(destination.clone(), Simulated::File(after));
                self.states
                    .insert(operation.source.clone(), Simulated::Missing);
                Ok(())
            }
        }
    }
}

fn mismatch_message(display: &str, failure: &HunkFailure) -> String {
    format!(
        "cannot patch \"{display}\": hunk at patch line {} ({}) does not match — {}",
        failure.line, failure.stage, failure.preview
    )
}

fn preflight(plan: &[ResolvedPatch], cancel: &Cancel) -> Result<(), ToolError> {
    let mut check = Preflight::new();
    let mut refusals = Vec::new();
    for operation in plan {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if let Err(message) = check.check(operation) {
            refusals.push(format!(
                "- operation {} ({}): {message}",
                operation.index + 1,
                operation.describe()
            ));
        }
    }
    if refusals.is_empty() {
        Ok(())
    } else {
        Err(ToolError::Failed(format!(
            "Patch rejected before any change ({} problems):\n{}",
            refusals.len(),
            refusals.join("\n")
        )))
    }
}

fn updated_text(operation: &ResolvedPatch, hunks: &[PatchHunk]) -> Result<String, String> {
    let text = std::fs::read_to_string(&operation.source)
        .map_err(|error| format!("cannot read \"{}\": {error}", operation.source_display))?;
    let text = normalize_lf(&text);
    if text.contains('\0') {
        return Err(format!(
            "cannot patch \"{}\": binary file",
            operation.source_display
        ));
    }
    apply_hunks(&text, hunks)
        .map_err(|failure| mismatch_message(&operation.source_display, &failure))
}

fn execute_one(operation: &ResolvedPatch) -> Result<(), String> {
    match &operation.operation {
        PatchOperation::Add { content, .. } => write_atomic(&operation.source, content, true)
            .map_err(|error| format!("cannot write \"{}\": {error}", operation.source_display)),
        PatchOperation::Update {
            hunks,
            move_to: Some(_),
            ..
        } => {
            let after = updated_text(operation, hunks)?;
            let Some((destination, destination_display)) = &operation.destination else {
                return Err("Move destination is missing.".into());
            };
            write_atomic(destination, &after, true)
                .map_err(|error| format!("cannot write \"{destination_display}\": {error}"))?;
            std::fs::remove_file(&operation.source).map_err(|error| {
                format!(
                    "cannot move \"{}\" to \"{destination_display}\": destination was written but the source could not be removed: {error}",
                    operation.source_display
                )
            })
        }
        PatchOperation::Update { hunks, .. } => {
            let after = updated_text(operation, hunks)?;
            write_atomic(&operation.source, &after, false)
                .map_err(|error| format!("cannot write \"{}\": {error}", operation.source_display))
        }
        PatchOperation::Delete { .. } => std::fs::remove_file(&operation.source)
            .map_err(|error| format!("cannot delete \"{}\": {error}", operation.source_display)),
    }
}

fn commit(plan: &[ResolvedPatch], cancel: &Cancel) -> Result<String, ToolError> {
    commit_operations(plan, cancel, execute_one)
}

fn commit_operations(
    plan: &[ResolvedPatch],
    cancel: &Cancel,
    mut execute: impl FnMut(&ResolvedPatch) -> Result<(), String>,
) -> Result<String, ToolError> {
    let mut completed: Vec<&ResolvedPatch> = Vec::new();
    for (index, operation) in plan.iter().enumerate() {
        if cancel.is_cancelled() {
            return if completed.is_empty() {
                Err(ToolError::Cancelled)
            } else {
                Err(ToolError::Failed(cancelled_output(plan, &completed, index)))
            };
        }
        match execute(operation) {
            Ok(()) => completed.push(operation),
            Err(message) => {
                return Err(ToolError::Failed(stopped_output(
                    plan, &completed, index, &message,
                )));
            }
        }
    }
    Ok(success_output(plan))
}

fn success_output(plan: &[ResolvedPatch]) -> String {
    if plan.is_empty() {
        return "Success. No operations in patch.".into();
    }
    let mut lines = vec!["Success. Updated the following files:".to_string()];
    push_completed(&mut lines, &plan.iter().collect::<Vec<_>>());
    lines.join("\n")
}

fn push_completed(lines: &mut Vec<String>, completed: &[&ResolvedPatch]) {
    for group in [Kind::Add, Kind::Update, Kind::Delete] {
        for operation in completed {
            let current = if operation.kind == Kind::Move {
                Kind::Update
            } else {
                operation.kind
            };
            if current == group {
                lines.push(operation.completed_line());
            }
        }
    }
}

fn stopped_output(
    plan: &[ResolvedPatch],
    completed: &[&ResolvedPatch],
    failed_index: usize,
    message: &str,
) -> String {
    let mut lines = vec![format!(
        "Patch stopped after {} of {} operations.",
        completed.len(),
        plan.len()
    )];
    if !completed.is_empty() {
        lines.push("Completed:".into());
        push_completed(&mut lines, completed);
    }
    lines.push("Failed:".into());
    let failed = &plan[failed_index];
    lines.push(format!(
        "- operation {} ({}): {message}",
        failed.index + 1,
        failed.describe()
    ));
    push_skipped(&mut lines, &plan[failed_index + 1..]);
    lines.join("\n")
}

fn push_skipped(lines: &mut Vec<String>, skipped: &[ResolvedPatch]) {
    if skipped.is_empty() {
        return;
    }
    lines.push("Not executed:".into());
    for operation in skipped {
        lines.push(format!(
            "- operation {} ({})",
            operation.index + 1,
            operation.describe_short()
        ));
    }
}

fn cancelled_output(
    plan: &[ResolvedPatch],
    completed: &[&ResolvedPatch],
    next_index: usize,
) -> String {
    let mut lines = vec![format!(
        "Patch cancelled after {} of {} operations.",
        completed.len(),
        plan.len()
    )];
    if !completed.is_empty() {
        lines.push("Completed:".into());
        push_completed(&mut lines, completed);
    }
    push_skipped(&mut lines, &plan[next_index..]);
    lines.join("\n")
}

pub struct ApplyPatch;

impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Create, update, delete, or move UTF-8 text files with one patch; prefer it over multiple edit_file calls when a change spans several files or separated regions."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Patch text: start with *** Begin Patch and end with *** End Patch. Use *** Add File: path with + lines; *** Update File: path, optionally followed by *** Move to: path, with @@ or @@ context and space-prefixed context, + added, or - removed lines; *** End of File anchors the last hunk to the file end. Use *** Delete File: path to delete. Paths are relative to the workspace."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Prefer apply_patch over separate edit_file calls for changes across files or separated regions. Give every hunk enough surrounding context lines to match one place in the current file. The whole patch is checked first (syntax, hunk matching, missing files) and a rejected patch changes nothing; once applying starts, operations run in order, stop at the first failure, and are not rolled back, so re-read affected files before retrying when needed."
    }

    fn summary(&self, args: &Value) -> String {
        let Some(input) = args.get("input").and_then(Value::as_str) else {
            return String::new();
        };
        let Ok(operations) = parse_patch(input) else {
            return String::new();
        };
        operations
            .iter()
            .take(3)
            .map(PatchOperation::path)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { apply_patch(ctx, args).map(ToolOutput::text) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    input: String,
}

fn apply_patch(ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
    let args: ApplyPatchArgs = parse_args(args)?;
    if args.input.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "input must contain a patch.".into(),
        ));
    }
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let operations = parse_patch(&args.input).map_err(|(line, reason)| {
        ToolError::InvalidArguments(format!("Invalid patch at line {line}: {reason}"))
    })?;
    let plan = plan(ctx, operations)?;
    preflight(&plan, &ctx.cancel)?;
    commit(&plan, &ctx.cancel)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        PatchOperation, apply_hunks, apply_patch, commit, commit_operations, execute_one,
        parse_patch, plan, preflight,
    };
    use crate::tools::ToolError;
    use crate::tools::test_support::context;

    fn run(ctx: &crate::tools::ToolContext, input: &str) -> Result<String, String> {
        apply_patch(ctx, json!({ "input": input })).map_err(|error| error.to_string())
    }

    fn text(path: std::path::PathBuf) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn parses_ordered_operations_and_move_endpoints() {
        let input = "\
*** Begin Patch
*** Add File: a.txt
+hello
+
*** Update File: b.txt
*** Move to: c.txt
@@ section
-old
+new
 same
*** Delete File: d.txt
*** End Patch";
        let operations = parse_patch(input).unwrap();
        assert_eq!(operations.len(), 3);
        match &operations[0] {
            PatchOperation::Add {
                path,
                line,
                content,
            } => {
                assert_eq!(path, "a.txt");
                assert_eq!(*line, 2);
                assert_eq!(content, "hello\n\n");
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        match &operations[1] {
            PatchOperation::Update {
                path,
                line,
                move_to,
                hunks,
            } => {
                assert_eq!(path, "b.txt");
                assert_eq!(*line, 5);
                assert_eq!(move_to.as_deref(), Some("c.txt"));
                assert_eq!(hunks[0].context.as_deref(), Some("section"));
                assert_eq!(hunks[0].old, vec!["old", "same"]);
                assert_eq!(hunks[0].new, vec!["new", "same"]);
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        assert!(matches!(
            &operations[2],
            PatchOperation::Delete { path, line } if path == "d.txt" && *line == 11
        ));
    }

    #[test]
    fn accepts_heredoc_wrappers_and_empty_patches() {
        let operations =
            parse_patch("\n<<'EOF'\n*** Begin Patch\n*** Add File: x\n+y\n*** End Patch\nEOF\n")
                .unwrap();
        assert_eq!(
            operations,
            vec![PatchOperation::Add {
                path: "x".into(),
                line: 4,
                content: "y\n".into(),
            }]
        );
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch").unwrap(),
            vec![]
        );
    }

    #[test]
    fn rejects_malformed_input_at_its_line() {
        let error = parse_patch("not a patch").unwrap_err();
        assert_eq!(error.0, 1);
        assert!(error.1.contains("Begin Patch"));

        let error = parse_patch("*** Begin Patch\n*** Update File: a\n*** End Patch").unwrap_err();
        assert_eq!(error.0, 2);
        assert!(error.1.contains("requires at least one hunk"));

        let error = parse_patch("*** Begin Patch\n*** Update File: a\nunprefixed\n*** End Patch")
            .unwrap_err();
        assert_eq!(error.0, 3);
        assert!(error.1.contains("context"));
    }

    #[test]
    fn matches_hunks_with_whitespace_tolerance_and_eof_anchoring() {
        let operations = parse_patch(
            "*** Begin Patch\n*** Update File: a\n@@ two\n-three\n+THREE\n*** End of File\n*** End Patch",
        )
        .unwrap();
        let PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update");
        };
        assert!(hunks[0].eof);
        assert_eq!(
            apply_hunks("one\ntwo\nthree\n", hunks).unwrap(),
            "one\ntwo\nTHREE\n"
        );

        let operations =
            parse_patch("*** Begin Patch\n*** Update File: a\n@@\n-alpha\n+ALPHA\n*** End Patch")
                .unwrap();
        let PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update");
        };
        assert_eq!(
            apply_hunks("alpha   \nbeta\n", hunks).unwrap(),
            "ALPHA\nbeta\n"
        );
    }

    #[test]
    fn applies_add_update_delete_and_move_together() {
        let (ctx, root, _handle) = context("patch-apply");
        std::fs::write(root.join("notes.txt"), "keep\nold\n").unwrap();
        std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(root.join("move.txt"), "old\n").unwrap();

        let output = run(
            &ctx,
            "\
*** Begin Patch
*** Add File: new.txt
+hello
*** Update File: notes.txt
@@ keep
-old
+new
*** Delete File: gone.txt
*** Update File: move.txt
*** Move to: moved.txt
@@
-old
+new
*** End Patch",
        )
        .unwrap();

        assert_eq!(
            output,
            "Success. Updated the following files:\nA new.txt\nM notes.txt\nM moved.txt\nD gone.txt"
        );
        assert_eq!(text(root.join("new.txt")), "hello\n");
        assert_eq!(text(root.join("notes.txt")), "keep\nnew\n");
        assert!(!root.join("gone.txt").exists());
        assert_eq!(text(root.join("moved.txt")), "new\n");
        assert!(!root.join("move.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn normalizes_crlf_before_patching() {
        let (ctx, root, _handle) = context("patch-crlf");
        std::fs::write(root.join("a.txt"), "one\r\ntwo\r\n").unwrap();

        run(
            &ctx,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n*** End Patch",
        )
        .unwrap();

        assert_eq!(text(root.join("a.txt")), "ONE\ntwo\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_the_whole_patch_before_writing() {
        let (ctx, root, _handle) = context("patch-reject");
        std::fs::write(root.join("a.txt"), "actual\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();

        let error = run(
            &ctx,
            "\
*** Begin Patch
*** Update File: a.txt
@@
-actual
+changed
*** Update File: b.txt
@@
-missing
+changed
*** End Patch",
        )
        .unwrap_err();

        assert!(error.contains("Patch rejected before any change (1 problems)"));
        assert!(error.contains("b.txt"));
        assert_eq!(text(root.join("a.txt")), "actual\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_stopped_patch_is_a_failure_even_after_writing_earlier_operations() {
        let (ctx, root, _handle) = context("patch-partial");
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
        )
        .unwrap();
        let planned = plan(&ctx, operations).unwrap();
        preflight(&planned, &ctx.cancel).unwrap();
        // The preflight passed, so the only remaining failure is a commit-time
        // race. Simulate one by creating the second target before commit.
        std::fs::write(root.join("second.txt"), "raced\n").unwrap();

        let error = commit(&planned, &ctx.cancel).unwrap_err().to_string();
        assert!(error.starts_with("Patch stopped after 1 of 2 operations."));
        assert!(error.contains("Completed:\nA first.txt"));
        assert!(error.contains("Failed:\n- operation 2 (add second.txt"));
        assert_eq!(text(root.join("first.txt")), "first\n");
        assert_eq!(text(root.join("second.txt")), "raced\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cancellation_before_commit_writes_nothing() {
        let (ctx, root, handle) = context("patch-cancel-before");
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
        )
        .unwrap();
        let planned = plan(&ctx, operations).unwrap();
        handle.cancel();

        assert!(matches!(
            preflight(&planned, &ctx.cancel),
            Err(ToolError::Cancelled)
        ));
        assert!(matches!(
            commit(&planned, &ctx.cancel),
            Err(ToolError::Cancelled)
        ));
        let error = run(
            &ctx,
            "*** Begin Patch\n*** Add File: first.txt\n+first\n*** End Patch",
        )
        .unwrap_err();
        assert_eq!(error, "Cancelled.");
        assert!(!root.join("first.txt").exists());
        assert!(!root.join("second.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cancellation_stops_before_the_next_operation() {
        let (ctx, root, handle) = context("patch-cancel-between");
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
        )
        .unwrap();
        let planned = plan(&ctx, operations).unwrap();
        preflight(&planned, &ctx.cancel).unwrap();

        let mut first = true;
        let error = commit_operations(&planned, &ctx.cancel, |operation| {
            let result = execute_one(operation);
            if first {
                first = false;
                handle.cancel();
            }
            result
        })
        .unwrap_err()
        .to_string();

        assert!(error.starts_with("Patch cancelled after 1 of 2 operations."));
        assert!(error.contains("Completed:\nA first.txt"));
        assert!(error.contains("Not executed:\n- operation 2 (add second.txt)"));
        assert_eq!(text(root.join("first.txt")), "first\n");
        assert!(!root.join("second.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_to_add_an_existing_file() {
        let (ctx, root, _handle) = context("patch-add");
        std::fs::write(root.join("a.txt"), "old\n").unwrap();

        let error = run(
            &ctx,
            "*** Begin Patch\n*** Add File: a.txt\n+new\n*** End Patch",
        )
        .unwrap_err();
        assert!(error.contains("file already exists"));
        assert_eq!(text(root.join("a.txt")), "old\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_patch_is_a_no_op() {
        let (ctx, root, _handle) = context("patch-empty");
        assert_eq!(
            run(&ctx, "*** Begin Patch\n*** End Patch").unwrap(),
            "Success. No operations in patch."
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_delete_a_symlink_source() {
        let (ctx, root, _handle) = context("patch-symlink");
        std::fs::write(root.join("target.txt"), "keep\n").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link")).unwrap();

        let error = run(
            &ctx,
            "*** Begin Patch\n*** Delete File: link\n*** End Patch",
        )
        .unwrap_err();

        assert!(error.contains("symbolic link"));
        assert!(root.join("link").exists());
        assert_eq!(text(root.join("target.txt")), "keep\n");
        std::fs::remove_dir_all(&root).ok();
    }
}
