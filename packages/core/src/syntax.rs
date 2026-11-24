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
    Python,
    Java,
}

impl Grammar {
    pub(crate) fn for_path(path: &str) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "rs" => Self::Rust,
            "js" | "jsx" | "cjs" | "mjs" => Self::JavaScript,
            "ts" | "cts" | "mts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "py" => Self::Python,
            "java" => Self::Java,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
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
        static PYTHON: OnceLock<Result<Query, String>> = OnceLock::new();
        static JAVA: OnceLock<Result<Query, String>> = OnceLock::new();
        let (cache, patterns) = match self {
            Self::Rust => (&RUST, RUST_DECLARATIONS),
            Self::JavaScript => (&JS, JS_DECLARATIONS),
            Self::TypeScript => (&TS, ""),
            Self::Tsx => (&TSX, ""),
            Self::Python => (&PYTHON, PYTHON_DECLARATIONS),
            Self::Java => (&JAVA, JAVA_DECLARATIONS),
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

const PYTHON_DECLARATIONS: &str = r#"
[
  (class_definition name: (identifier) @name)
  (function_definition name: (identifier) @name)
] @declaration
"#;

const JAVA_DECLARATIONS: &str = r#"
[
  (class_declaration name: (identifier) @name)
  (interface_declaration name: (identifier) @name)
  (enum_declaration name: (identifier) @name)
  (record_declaration name: (identifier) @name)
  (annotation_type_declaration name: (identifier) @name)
  (annotation_type_element_declaration name: (identifier) @name)
  (constructor_declaration name: (identifier) @name)
  (compact_constructor_declaration name: (identifier) @name)
  (method_declaration name: (identifier) @name)
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

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Structure {
    InvokeCall,
    TauriCommand,
    RustTest,
}

impl Structure {
    pub(crate) fn supports(self, grammar: Grammar) -> bool {
        match self {
            Self::InvokeCall => matches!(
                grammar,
                Grammar::JavaScript | Grammar::TypeScript | Grammar::Tsx
            ),
            Self::TauriCommand | Self::RustTest => grammar == Grammar::Rust,
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::InvokeCall => "invoke call",
            Self::TauriCommand => "Tauri command definition",
            Self::RustTest => "Rust test definition",
        }
    }

    pub(crate) fn lines(
        self,
        grammar: Grammar,
        source: &str,
        limit: usize,
        interrupted: impl Fn() -> bool,
    ) -> Result<Vec<usize>, String> {
        let tree = grammar.parse(source, &interrupted)?;
        let root = tree.root_node();
        let mut result = Vec::new();
        walk_nodes(root, |node| {
            if interrupted() {
                return false;
            }
            let found = match self {
                Self::InvokeCall => invoke_call(node, source).then_some(node),
                Self::TauriCommand | Self::RustTest => self.attribute_function(node, source),
            };
            if let Some(found) = found {
                result.push(found.start_position().row + 1);
            }
            result.len() < limit
        });
        if interrupted() {
            return Err("Syntax analysis was interrupted.".into());
        }
        Ok(result)
    }

    fn attribute_function<'tree>(self, node: Node<'tree>, source: &str) -> Option<Node<'tree>> {
        if node.kind() != "attribute_item" {
            return None;
        }
        let attribute = node.named_child(0)?.named_child(0)?;
        let identifier_is = |node: Node<'_>, expected: &str| &source[node.byte_range()] == expected;
        let matches = match self {
            Self::TauriCommand => {
                attribute.kind() == "scoped_identifier"
                    && attribute
                        .child_by_field_name("path")
                        .is_some_and(|node| identifier_is(node, "tauri"))
                    && attribute
                        .child_by_field_name("name")
                        .is_some_and(|node| identifier_is(node, "command"))
            }
            Self::RustTest => attribute.kind() == "identifier" && identifier_is(attribute, "test"),
            Self::InvokeCall => false,
        };
        if !matches {
            return None;
        }
        let mut next = node.next_named_sibling()?;
        while matches!(
            next.kind(),
            "attribute_item" | "line_comment" | "block_comment"
        ) {
            next = next.next_named_sibling()?;
        }
        (next.kind() == "function_item").then_some(next)
    }
}

fn invoke_call(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(callee) = node.child_by_field_name("function") else {
        return false;
    };
    let name = match callee.kind() {
        "identifier" => Some(callee),
        "member_expression" => callee.child_by_field_name("property"),
        _ => None,
    };
    name.is_some_and(|name| &source[name.byte_range()] == "invoke")
}

pub(crate) fn walk_nodes(root: Node<'_>, mut visit: impl FnMut(Node<'_>) -> bool) {
    let mut cursor = root.walk();
    loop {
        if !visit(cursor.node()) {
            return;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
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
        if matches!(parent.kind(), "export_statement" | "decorated_definition") {
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
        "function_definition" => {
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                match parent.kind() {
                    "class_definition" => return "method",
                    "function_definition" => break,
                    _ => ancestor = parent.parent(),
                }
            }
            "function"
        }
        "method_definition"
        | "method_signature"
        | "abstract_method_signature"
        | "method_declaration"
        | "annotation_type_element_declaration" => "method",
        "constructor_declaration" | "compact_constructor_declaration" => "constructor",
        "struct_item" => "struct",
        "enum_item" | "enum_declaration" => "enum",
        "union_item" => "union",
        "trait_item" => "trait",
        "type_item" | "associated_type" | "type_alias_declaration" => "type",
        "mod_item" | "internal_module" => "module",
        "class_declaration" | "abstract_class_declaration" | "class_definition" => "class",
        "interface_declaration" => "interface",
        "record_declaration" => "record",
        "annotation_type_declaration" => "annotation",
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
            | "function_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "class_definition"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "method_declaration"
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
