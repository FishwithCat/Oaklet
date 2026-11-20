//! Patch grammar: markers, hunks, and the operations they describe.

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const EOF: &str = "*** End of File";
const MOVE: &str = "*** Move to: ";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";

pub(super) type ParseError = (usize, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PatchHunk {
    /// One-based input line containing the marker or first implicit hunk line.
    pub(super) line: usize,
    /// Text after `@@ `, used as a search anchor.
    pub(super) context: Option<String>,
    /// Context and removed lines without prefixes.
    pub(super) old: Vec<String>,
    /// Context and added lines without prefixes.
    pub(super) new: Vec<String>,
    /// Whether `*** End of File` follows this hunk.
    pub(super) eof: bool,
}

impl PatchHunk {
    pub(super) fn new(line: usize) -> Self {
        Self {
            line,
            context: None,
            old: Vec::new(),
            new: Vec::new(),
            eof: false,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.old.is_empty() && self.new.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PatchOperation {
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
    pub(super) fn path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Delete { path, .. } | Self::Update { path, .. } => path,
        }
    }
}

/// Parse a complete patch without touching the filesystem.
pub(super) fn parse_patch(input: &str) -> Result<Vec<PatchOperation>, ParseError> {
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
