use std::collections::HashSet;
use std::fmt::Write;

use tree_sitter::{Node, Tree};

use crate::syntax::{Grammar, walk_nodes};

use super::diff::line_changes;

pub(super) fn new_warnings(
    path: &str,
    before: Option<(&str, &str, Option<&Tree>)>,
    after: &str,
    tree: Option<&Tree>,
) -> Vec<String> {
    let Some(tree) = tree.filter(|tree| tree.root_node().has_error()) else {
        return Vec::new();
    };
    let before = before.filter(|(name, _, _)| Grammar::for_path(name) == Grammar::for_path(path));
    let mut previous = HashSet::new();
    let mut old_source = "";
    if let Some((name, source, cached)) = before {
        old_source = source;
        let parsed = cached
            .is_none()
            .then(|| Grammar::for_path(name)?.parse(source, || false).ok())
            .flatten();
        if let Some(previous_tree) = cached.or(parsed.as_ref())
            && previous_tree.root_node().has_error()
        {
            walk_nodes(previous_tree.root_node(), |node| {
                if let Some(kind) = error_kind(node) {
                    previous.insert((node.start_position(), kind));
                }
                true
            });
        }
    }
    let changes = line_changes(old_source, after);
    let mut output = Vec::new();
    walk_nodes(tree.root_node(), |node| {
        let Some(kind) = error_kind(node) else {
            return true;
        };
        let start = node.start_position();
        let end = node.end_position();
        let last = if end.column == 0 && end.row > start.row {
            end.row - 1
        } else {
            end.row
        };
        if changes[start.row..=last]
            .iter()
            .all(|&(_, changed)| !changed)
        {
            return true;
        }
        if let Some(old_row) = changes[start.row].0
            && previous.contains(&(
                tree_sitter::Point {
                    row: old_row,
                    column: start.column,
                },
                kind,
            ))
        {
            return true;
        }
        let message = if node.is_missing() {
            format!("missing `{kind}`")
        } else {
            "unexpected syntax".into()
        };
        let column = after[node.start_byte() - start.column..node.start_byte()]
            .chars()
            .count()
            + 1;
        output.push(format!("{path}:{}:{column}: {message}", start.row + 1));
        output.len() <= 6
    });
    output
}

fn error_kind(node: Node<'_>) -> Option<&'static str> {
    if node.is_error() || node.is_missing() {
        Some(node.kind())
    } else {
        None
    }
}

pub(super) fn append_warnings(output: &mut String, warnings: Vec<String>) {
    let count = warnings.len();
    if count == 0 {
        return;
    }
    output.push_str("\n\nPossible syntax errors introduced by this edit:");
    for warning in warnings.iter().take(6) {
        let _ = write!(output, "\n- {warning}");
    }
    if count > 6 {
        output.push_str("\n- Additional diagnostics omitted.");
    }
}
