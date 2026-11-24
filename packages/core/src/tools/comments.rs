use std::collections::HashMap;

use tree_sitter::Tree;

use crate::syntax::Grammar;

const LISTED: usize = 6;

pub(super) fn checks_path(path: &str) -> bool {
    Grammar::for_path(path).is_some()
}

fn parsed_comments<'a>(grammar: Grammar, text: &'a str, tree: &Tree) -> Vec<(usize, &'a str)> {
    let mut cursor = tree.walk();
    let mut comments = Vec::new();
    let rust = grammar == Grammar::Rust;
    loop {
        let node = cursor.node();
        if matches!(node.kind(), "comment" | "line_comment" | "block_comment") {
            let comment = text[node.byte_range()].trim();
            if !is_directive(comment, rust) {
                comments.push((node.start_position().row + 1, comment));
            }
        } else if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return comments;
            }
        }
    }
}

fn is_directive(comment: &str, rust: bool) -> bool {
    if rust {
        return comment.starts_with("// SAFETY:");
    }
    [
        "// eslint-",
        "//eslint-",
        "/* eslint",
        "// @ts-",
        "// @jsx",
        "// prettier-ignore",
        "// biome-ignore",
        "/* @__PURE__",
        "/*#__PURE__",
    ]
    .iter()
    .any(|prefix| comment.starts_with(prefix))
}

pub(super) struct EditSyntax {
    pub(super) before: Option<Tree>,
    pub(super) after: Option<Tree>,
}

pub(super) fn check_edit(
    path: &str,
    before: Option<(&str, &str)>,
    after: &str,
    previous_tree: Option<&Tree>,
) -> Result<EditSyntax, String> {
    let Some(after_language) = Grammar::for_path(path) else {
        return Ok(EditSyntax {
            before: None,
            after: None,
        });
    };
    let after_tree = after_language.parse(after, || false)?;
    let comments = parsed_comments(after_language, after, &after_tree);
    let mut before_tree = None;
    if let Some((before_path, before_text)) = before
        && let Some(before_language) = Grammar::for_path(before_path)
        && (!comments.is_empty() || after_tree.root_node().has_error())
    {
        before_tree = Some(match previous_tree {
            Some(tree) => tree.clone(),
            None => before_language.parse(before_text, || false)?,
        });
    }
    if comments.is_empty() {
        return Ok(EditSyntax {
            before: before_tree,
            after: Some(after_tree),
        });
    }
    let mut existing = HashMap::new();
    if let Some((before_path, before_text)) = before
        && let Some(before_language) = Grammar::for_path(before_path)
        && let Some(tree) = &before_tree
    {
        for (_, comment) in parsed_comments(before_language, before_text, tree) {
            *existing
                .entry(comment.lines().map(str::trim).collect::<Vec<_>>())
                .or_insert(0usize) += 1;
        }
    }
    let mut added = 0;
    let mut listed = Vec::new();
    for (line, comment) in comments {
        let key: Vec<_> = comment.lines().map(str::trim).collect();
        if let Some(count) = existing.get_mut(&key)
            && *count > 0
        {
            *count -= 1;
            continue;
        }
        added += 1;
        if listed.len() < LISTED {
            listed.push(format!("- line {line}: {}", key[0]));
        }
    }
    if added == 0 {
        return Ok(EditSyntax {
            before: before_tree,
            after: Some(after_tree),
        });
    }
    if added > LISTED {
        listed.push(format!("- ...and {} more", added - LISTED));
    }
    Err(format!(
        "Refusing to write {path}: it adds {added} comment(s). New code comments are not \
         allowed, including doc comments. Remove the listed comments and call the tool again:\n{}",
        listed.join("\n"),
    ))
}

#[cfg(test)]
mod tests {
    use super::check_edit;

    fn refuse_new_comments(
        path: &str,
        before: Option<(&str, &str)>,
        after: &str,
    ) -> Result<(), String> {
        check_edit(path, before, after, None).map(|_| ())
    }

    #[test]
    fn reports_actual_comments_with_line_numbers() {
        let path = "src/app.ts";
        let literal = "const text = `\n// literal text\n`;\n";
        assert!(refuse_new_comments(path, None, literal).is_ok());
        let error = refuse_new_comments(
            path,
            Some((path, literal)),
            "const text = '';\n// literal text\nexport const app = 1; /* details */\n",
        )
        .unwrap_err();
        assert!(error.contains("line 2: // literal text"), "{error}");
        assert!(error.contains("line 3: /* details */"), "{error}");
    }

    #[test]
    fn allows_reindenting_existing_comments_but_rejects_duplicates() {
        let path = "src/lib.rs";
        let before = "fn run() {\n    /// existing\n    fn nested() {}\n}\n";
        let reindented = "fn run() {\n        /// existing\n        fn nested() {}\n}\n";
        assert!(refuse_new_comments(path, Some((path, before)), reindented).is_ok());
        let duplicate = reindented.replace("/// existing", "/// existing\n        /// existing");
        assert!(refuse_new_comments(path, Some((path, before)), &duplicate).is_err());
    }

    #[test]
    fn preserves_shebangs_directives_and_non_code_files() {
        assert!(refuse_new_comments("cli.js", None, "#!/usr/bin/env node\n").is_ok());
        assert!(refuse_new_comments("src/app.ts", None, "// @ts-expect-error\nrun();\n").is_ok());
        assert!(refuse_new_comments("main.rs", None, "// SAFETY: validated above\n").is_ok());
        assert!(refuse_new_comments("notes.md", None, "# Heading\n// example\n").is_ok());
    }
}
