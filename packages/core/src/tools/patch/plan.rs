//! Resolve patch paths against the workspace and check every hunk before
//! anything is written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::apply::{apply_hunks, mismatch_message, normalize_lf};
use super::parse::PatchOperation;
use crate::tools::fs::MAX_FILE_BYTES;
use crate::tools::{Cancel, ToolContext, ToolError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Add,
    Update,
    Delete,
    Move,
}

pub(super) struct ResolvedPatch {
    pub(super) index: usize,
    pub(super) kind: Kind,
    pub(super) operation: PatchOperation,
    pub(super) source: PathBuf,
    pub(super) source_display: String,
    pub(super) destination: Option<(PathBuf, String)>,
}

impl ResolvedPatch {
    pub(super) fn line(&self) -> usize {
        match &self.operation {
            PatchOperation::Add { line, .. }
            | PatchOperation::Delete { line, .. }
            | PatchOperation::Update { line, .. } => *line,
        }
    }

    pub(super) fn destination_display(&self) -> &str {
        self.destination
            .as_ref()
            .map(|(_, display)| display.as_str())
            .unwrap_or(&self.source_display)
    }

    /// `update path, patch line 2`, with move endpoints when applicable.
    pub(super) fn describe(&self) -> String {
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

    pub(super) fn describe_short(&self) -> String {
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

    pub(super) fn completed_line(&self) -> String {
        match self.kind {
            Kind::Add => format!("A {}", self.source_display),
            Kind::Update => format!("M {}", self.source_display),
            Kind::Delete => format!("D {}", self.source_display),
            Kind::Move => format!("M {}", self.destination_display()),
        }
    }
}

pub(super) fn plan(
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

pub(super) fn preflight(plan: &[ResolvedPatch], cancel: &Cancel) -> Result<(), ToolError> {
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
