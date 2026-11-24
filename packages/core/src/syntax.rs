use std::ops::ControlFlow;
use std::path::Path;
use std::sync::OnceLock;

use serde::Serialize;
use tree_sitter::{
    Language, Node, ParseOptions, Parser, Query, QueryCursor, StreamingIterator, Tree,
};

pub(crate) const MAX_SOURCE_BYTES: usize = 2_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grammar {
    Rust,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Grammar {
    pub(crate) fn for_path(path: &str) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "rs" => Self::Rust,
            "js" | "jsx" | "cjs" | "mjs" => Self::JavaScript,
            "ts" | "cts" | "mts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    pub(crate) fn parse(
        self,
        source: &str,
        interrupted: impl Fn() -> bool,
    ) -> Result<Tree, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|error| format!("Cannot initialize the syntax parser: {error}"))?;
        let mut check = |_: &_| {
            if interrupted() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        parser
            .parse_with_options(
                &mut |offset, _| &source.as_bytes()[offset.min(source.len())..],
                None,
                Some(ParseOptions::new().progress_callback(&mut check)),
            )
            .ok_or_else(|| "Syntax parsing was interrupted.".into())
    }

    fn declarations(self) -> Result<&'static Query, String> {
        static RUST: OnceLock<Result<Query, String>> = OnceLock::new();
        static JS: OnceLock<Result<Query, String>> = OnceLock::new();
        static TS: OnceLock<Result<Query, String>> = OnceLock::new();
        static TSX: OnceLock<Result<Query, String>> = OnceLock::new();
        let (cache, patterns) = match self {
            Self::Rust => (&RUST, RUST_DECLARATIONS),
            Self::JavaScript => (&JS, JS_DECLARATIONS),
            Self::TypeScript => (&TS, ""),
            Self::Tsx => (&TSX, ""),
        };
        cache
            .get_or_init(|| {
                let typed;
                let patterns = if matches!(self, Self::TypeScript | Self::Tsx) {
                    typed = format!("{JS_DECLARATIONS}\n{TS_DECLARATIONS}");
                    typed.as_str()
                } else {
                    patterns
                };
                Query::new(&self.language(), patterns).map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

const RUST_DECLARATIONS: &str = r#"
[
  (function_item name: (_) @name)
  (function_signature_item name: (_) @name)
  (struct_item name: (_) @name)
  (enum_item name: (_) @name)
  (union_item name: (_) @name)
  (trait_item name: (_) @name)
  (type_item name: (_) @name)
  (associated_type name: (_) @name)
  (mod_item name: (_) @name)
] @declaration
"#;

const JS_DECLARATIONS: &str = r#"
[
  (function_declaration name: (_) @name)
  (generator_function_declaration name: (_) @name)
  (class_declaration name: (_) @name)
  (method_definition name: (_) @name)
  (variable_declarator name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function) (class)])
] @declaration
"#;

const TS_DECLARATIONS: &str = r#"
[
  (abstract_class_declaration name: (_) @name)
  (function_signature name: (_) @name)
  (method_signature name: (_) @name)
  (abstract_method_signature name: (_) @name)
  (interface_declaration name: (_) @name)
  (type_alias_declaration name: (_) @name)
  (enum_declaration name: (_) @name)
  (internal_module name: (_) @name)
] @declaration
"#;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Symbol {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) container: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Outline {
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) has_errors: bool,
    pub(crate) truncated: bool,
}

pub(crate) fn outline(
    grammar: Grammar,
    source: &str,
    limit: usize,
    interrupted: impl Fn() -> bool,
) -> Result<Outline, String> {
    let tree = grammar.parse(source, &interrupted)?;
    let root = tree.root_node();
    let query = grammar.declarations()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source.as_bytes());
    let mut symbols = Vec::new();
    let mut truncated = false;
    while let Some(found) = matches.next() {
        if interrupted() {
            return Err("Syntax analysis was interrupted.".into());
        }
        let [first, second] = found.captures else {
            continue;
        };
        let (declaration, name) = if query.capture_names()[first.index as usize] == "declaration" {
            (first.node, second.node)
        } else {
            (second.node, first.node)
        };
        if name.is_missing() {
            continue;
        }
        if symbols.len() == limit {
            truncated = true;
            break;
        }
        let start = declaration_start(declaration);
        let end = declaration.end_position();
        symbols.push(Symbol {
            name: label(name, source),
            kind: symbol_kind(declaration),
            container: container(declaration, source),
            start_line: start,
            end_line: (end.row + usize::from(end.column != 0)).max(start),
        });
    }
    if interrupted() {
        return Err("Syntax analysis was interrupted.".into());
    }
    symbols.sort_by_key(|symbol| (symbol.start_line, std::cmp::Reverse(symbol.end_line)));
    Ok(Outline {
        symbols,
        has_errors: root.has_error(),
        truncated,
    })
}

fn declaration_start(node: Node<'_>) -> usize {
    let mut start = node;
    if let Some(parent) = node.parent() {
        if parent.kind() == "export_statement" {
            start = parent;
        } else if matches!(
            parent.kind(),
            "lexical_declaration" | "variable_declaration"
        ) && node.prev_named_sibling().is_none()
        {
            start = parent;
            if let Some(export) = parent
                .parent()
                .filter(|node| node.kind() == "export_statement")
            {
                start = export;
            }
        }
    }
    while let Some(attribute) = start
        .prev_named_sibling()
        .filter(|node| node.kind() == "attribute_item")
    {
        start = attribute;
    }
    start.start_position().row + 1
}

fn symbol_kind(node: Node<'_>) -> &'static str {
    match node.kind() {
        "function_item" | "function_signature_item"
            if node
                .parent()
                .and_then(|body| body.parent())
                .is_some_and(|parent| matches!(parent.kind(), "impl_item" | "trait_item")) =>
        {
            "method"
        }
        "method_definition" | "method_signature" | "abstract_method_signature" => "method",
        "struct_item" => "struct",
        "enum_item" | "enum_declaration" => "enum",
        "union_item" => "union",
        "trait_item" => "trait",
        "type_item" | "associated_type" | "type_alias_declaration" => "type",
        "mod_item" | "internal_module" => "module",
        "class_declaration" | "abstract_class_declaration" => "class",
        "interface_declaration" => "interface",
        "variable_declarator"
            if node
                .child_by_field_name("value")
                .is_some_and(|value| value.kind() == "class") =>
        {
            "class"
        }
        _ => "function",
    }
}

fn container(node: Node<'_>, source: &str) -> String {
    let mut parents = Vec::new();
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        let name = match parent.kind() {
            "impl_item" => {
                if let Some(target) = parent.child_by_field_name("type") {
                    let target = label(target, source);
                    let name = parent.child_by_field_name("trait").map_or_else(
                        || target.clone(),
                        |trait_node| format!("{} for {target}", label(trait_node, source)),
                    );
                    parents.push(name);
                }
                None
            }
            "function_item"
            | "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "method_definition"
            | "mod_item"
            | "trait_item"
            | "internal_module" => parent.child_by_field_name("name"),
            "variable_declarator"
                if parent.child_by_field_name("value").is_some_and(|value| {
                    matches!(
                        value.kind(),
                        "arrow_function" | "function_expression" | "generator_function" | "class"
                    )
                }) =>
            {
                parent.child_by_field_name("name")
            }
            _ => None,
        };
        if let Some(name) = name {
            parents.push(label(name, source));
        }
        if parents.len() == 8 {
            break;
        }
        ancestor = parent.parent();
    }
    parents.reverse();
    parents.join(" / ")
}

fn label(node: Node<'_>, source: &str) -> String {
    let mut chars = source[node.byte_range()].chars();
    let mut label: String = chars
        .by_ref()
        .take(120)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}
