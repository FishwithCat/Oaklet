//! Model positions are one-based Unicode characters; service positions are UTF-16.
use super::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, Workspace, parse_args};
use crate::code_index::{
    CodeIndexManager, IndexError, Language, Location, Query, QueryData, QueryResult, ServerState,
    documents,
};
use futures_util::future::BoxFuture;
use lsp_types::Position;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const GUIDANCE: &str = "Use code_definition, code_references, and code_calls for exact semantic locations after identifying the symbol. For edits that affect a symbol's uses, call code_references on the declaration to distinguish same-named symbols. Verify additional text candidates against their imports and types. Keep grep for strings, macros, configuration, generated code and framework conventions. code_symbols with query searches every ready project and needs no path; without query it lists one file with exact positions, which code_definition, code_references and code_calls need (1-based line, 1-based Unicode character column). code_calls returns one hop and keeps prepared subjects and same-name candidates; only server call hierarchy results are call evidence. Results may be partial even without truncation, and protocol readiness does not prove semantic completeness. Inspect file_counts first; use reference_file to expand one file and next_offset to fetch more entries. Treat uncovered text matches as candidates and verify them against imports and types. External targets are metadata only (UTF-16 columns). Oversized output is spilled to a file readable with read_file offset/limit.";

pub(crate) fn register(registry: &mut ToolRegistry, manager: Arc<CodeIndexManager>) {
    for name in [
        "code_symbols",
        "code_definition",
        "code_references",
        "code_calls",
    ] {
        registry.push(Box::new(CodeTool {
            name,
            manager: manager.clone(),
        }));
    }
}
struct CodeTool {
    name: &'static str,
    manager: Arc<CodeIndexManager>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: Option<String>,
    query: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    direction: Option<Direction>,
    limit: Option<usize>,
    offset: Option<usize>,
    reference_file: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Incoming,
    Outgoing,
}

impl Tool for CodeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        match self.name {
            "code_symbols" => {
                "Start here when you know a symbol name but not its file, and to get the declaration position that code_references needs. query searches every ready project; path narrows the search to that file's project, and without query path lists that file. When returned names collide, a hint points at code_references; completeness is unknown."
            }
            "code_definition" => {
                "Use when a bare name may bind elsewhere (imports, re-exports, traits, shadowing) and text search cannot tell which declaration applies. Preserves multiple candidates; external targets are reported without reading them. When truncated, page with offset."
            }
            "code_references" => {
                "Call before renaming or editing a name that may also exist in another type, crate or import. Returns only locations bound to this declaration, excluding the declaration itself, but may omit other locations bound to it. Start with file_counts, then use reference_file for details in a specific file. Use next_offset if more entries are needed. Partial results do not establish absence."
            }
            _ => {
                "Use for callers or callees of one function when grep would confuse same-name functions. Server call hierarchy only: one hop, preserving recursive calls, prepared subjects and distinct candidates; unsupported is not an empty call graph."
            }
        }
    }
    fn parameters(&self) -> Value {
        let mut properties = json!({
            "path": {"type":"string", "minLength":1, "description": if self.name == "code_symbols" {"Optional saved file. With query it selects a project; omit it to search every ready project. Without query it lists this file's symbols."} else {"Saved source file in this session's workspace; selects the project for symbol search."}},
            "limit": {"type":"integer", "minimum":1, "maximum":200, "default":50, "description":"Maximum entries; call sites share this budget."}
        });
        let mut required = Vec::new();
        if self.name != "code_symbols" {
            required.push("path");
        }
        if self.name == "code_symbols" {
            properties["query"] = json!({"type":"string", "minLength":1, "maxLength":1024, "description":"Project symbol search (server matching rules). Omit to list one file's symbols."});
        } else {
            properties["line"] =
                json!({"type":"integer","minimum":1, "description":"One-based saved-source line."});
            properties["column"] = json!({"type":"integer","minimum":1, "description":"One-based Unicode character column."});
            required.extend(["line", "column"]);
        }
        if self.name == "code_calls" {
            properties["direction"] = json!({"type":"string", "enum":["incoming","outgoing"]});
            required.push("direction");
        }
        if matches!(self.name, "code_definition" | "code_references") {
            properties["offset"] = json!({"type":"integer","minimum":0,"maximum":1000000,"default":0,"description":"Number of location entries to skip; use with limit to page through truncated results."});
        }
        if self.name == "code_references" {
            properties["limit"]["default"] = json!(12);
            properties["reference_file"] = json!({"type":"string","minLength":1,"description":"Return references within this workspace file only. Keep path, line and column at the original symbol."});
        }
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn guidance(&self) -> &str {
        GUIDANCE
    }
    fn prompt_status(&self, workspace: &Path) -> Option<String> {
        if self.name != "code_symbols" {
            return None;
        }
        index_note(&self.manager, workspace)
    }
    fn summary(&self, args: &Value) -> String {
        match args.get("line").and_then(Value::as_u64) {
            Some(line) => format!(
                "{}:{line}:{} {}",
                args["path"].as_str().unwrap_or_default(),
                args["column"],
                args["direction"].as_str().unwrap_or_default()
            )
            .trim()
            .into(),
            None => format!(
                "{} {}",
                args["path"].as_str().unwrap_or_default(),
                args["query"].as_str().unwrap_or_default()
            )
            .trim()
            .into(),
        }
    }
    fn is_concurrency_safe(&self) -> bool {
        true
    }
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        value: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: Args = parse_args(value)?;
            validate(self.name, &args)?;
            let references = self.name == "code_references";
            let limit = args.limit.unwrap_or(if references { 12 } else { 50 });
            let offset = args.offset.unwrap_or(0);
            let work = async {
                let reference_file = args
                    .reference_file
                    .as_deref()
                    .map(|path| ctx.workspace.resolve(path))
                    .transpose()
                    .map_err(IndexError::Invalid)?;
                let format = if references {
                    ResultFormat::References(reference_file.as_deref())
                } else {
                    ResultFormat::Standard
                };
                let resolved = args
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(|path| ctx.workspace.resolve(path))
                    .transpose()
                    .map_err(IndexError::Invalid)?;
                let file = resolved
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                let mut input_hash = None;
                let query_mode = self.name == "code_symbols" && args.query.is_some();
                let query = if self.name == "code_symbols" {
                    match args.query {
                        Some(query) => Query::WorkspaceSymbols { file, query },
                        None => Query::Symbols {
                            file: file
                                .ok_or_else(|| IndexError::Invalid("path is required".into()))?,
                        },
                    }
                } else {
                    let path = resolved
                        .as_ref()
                        .ok_or_else(|| IndexError::Invalid("path is required".into()))?;
                    let text = documents::read(ctx.workspace.root(), path).await?;
                    input_hash = Some(blake3::hash(text.as_bytes()).to_hex().to_string());
                    let position = documents::convert(
                        &text,
                        Position::new(args.line.unwrap() - 1, args.column.unwrap() - 1),
                        "utf-32",
                        "utf-16",
                    )?;
                    let file =
                        file.ok_or_else(|| IndexError::Invalid("path is required".into()))?;
                    match self.name {
                        "code_definition" => Query::Definition {
                            file,
                            line: position.line,
                            column: position.character,
                        },
                        "code_references" => Query::References {
                            file,
                            line: position.line,
                            column: position.character,
                        },
                        _ => Query::Calls {
                            file,
                            line: position.line,
                            column: position.character,
                            incoming: matches!(args.direction, Some(Direction::Incoming)),
                        },
                    }
                };
                let result = self
                    .manager
                    .query(ctx.workspace.root(), query.clone(), &ctx.cancel)
                    .await;
                let result = if references
                    && matches!(
                        result,
                        Err(IndexError::NotReady
                            | IndexError::NotReadyReason(_)
                            | IndexError::Busy)
                    ) {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    self.manager
                        .query(ctx.workspace.root(), query, &ctx.cancel)
                        .await?
                } else {
                    result?
                };
                if let Some(hash) = input_hash {
                    let path = resolved
                        .as_ref()
                        .ok_or_else(|| IndexError::Invalid("path is required".into()))?;
                    let current = documents::read(ctx.workspace.root(), path).await?;
                    if result.content_hash.as_ref() != Some(&hash)
                        || blake3::hash(current.as_bytes()).to_hex().as_str() != hash
                    {
                        return Err(IndexError::NotReadyReason("file_changed"));
                    }
                }
                let mut value = render(&ctx.workspace, result, limit, offset, format).await?;
                if query_mode {
                    add_duplicate_hint(&mut value);
                }
                Ok(value)
            };
            let result = tokio::select! { biased;
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                result = work => result,
            };
            match result {
                Ok(value) => Ok(ToolOutput::text(if references {
                    value.to_string()
                } else {
                    serde_json::to_string_pretty(&value)
                        .map_err(|e| ToolError::Failed(e.to_string()))?
                })),
                Err(IndexError::Cancelled) => Err(ToolError::Cancelled),
                Err(error) => Err(failure(error)),
            }
        })
    }
}
fn validate(name: &str, args: &Args) -> Result<(), ToolError> {
    let path = args
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let invalid = if !(1..=200).contains(&args.limit.unwrap_or(50)) {
        Some("limit must be 1..200")
    } else if args.offset.is_some_and(|offset| offset > 1_000_000) {
        Some("offset must be at most 1000000")
    } else if args.offset.is_some() && !matches!(name, "code_definition" | "code_references") {
        Some("offset is only supported by code_definition and code_references")
    } else if args
        .reference_file
        .as_ref()
        .is_some_and(|path| name != "code_references" || path.trim().is_empty())
    {
        Some("reference_file requires a nonempty path and code_references")
    } else if name == "code_symbols" {
        if args.line.is_some() || args.column.is_some() || args.direction.is_some() {
            Some("symbol queries do not accept positions or direction")
        } else if args
            .query
            .as_ref()
            .is_some_and(|q| q.trim().is_empty() || q.chars().count() > 1024)
        {
            Some("query must contain 1..1024 characters")
        } else if path.is_none() && args.query.is_none() {
            Some("path is required unless query is set")
        } else {
            None
        }
    } else if path.is_none() {
        Some("path is required")
    } else if args.query.is_some() {
        Some("query is only supported by code_symbols")
    } else if args.line.unwrap_or(0) == 0 || args.column.unwrap_or(0) == 0 {
        Some("line and column must be positive integers")
    } else if name == "code_calls" && args.direction.is_none() {
        Some("code_calls requires direction")
    } else if name != "code_calls" && args.direction.is_some() {
        Some("direction is only supported by code_calls")
    } else {
        None
    };
    invalid.map_or(Ok(()), |message| {
        Err(ToolError::InvalidArguments(message.into()))
    })
}
fn failure(error: IndexError) -> ToolError {
    let status = match &error {
        IndexError::Busy | IndexError::NotReady | IndexError::NotReadyReason(_) => "not_ready",
        IndexError::NoReferences => "no_results",
        IndexError::Closed | IndexError::Disabled | IndexError::Backend(_) => "unavailable",
        IndexError::Unsupported(_) => "unsupported",
        IndexError::Timeout => "timeout",
        IndexError::Invalid(_) => "invalid_arguments",
        IndexError::Cancelled => "cancelled",
    };
    let reason = match &error {
        IndexError::NotReadyReason(reason) => Some(*reason),
        IndexError::Busy => Some("server_busy"),
        IndexError::NoReferences => Some("empty_backend_result"),
        _ => None,
    };
    ToolError::Failed(json!({"status":status,"reason":reason,"detail":error.to_string(),"completeness_known":false,"guidance":"Use grep/read_file as fallback; text matches are candidates, not confirmed calls. No result here proves absence."}).to_string())
}
struct Locations<'a> {
    workspace: &'a Workspace,
    texts: BTreeMap<PathBuf, String>,
}
impl Locations<'_> {
    async fn render(&mut self, location: &Location) -> Result<Value, IndexError> {
        // Resolve symlinks too: a lexical workspace prefix alone does not authorize a read.
        if self
            .workspace
            .resolve(&location.path.to_string_lossy())
            .is_err()
        {
            return Ok(
                json!({"path":location.path,"external":true,"line":u64::from(location.line)+1,"column":u64::from(location.column)+1,"end_line":u64::from(location.end_line)+1,"end_column":u64::from(location.end_column)+1,"column_encoding":"utf-16"}),
            );
        }
        if !self.texts.contains_key(&location.path) {
            self.texts.insert(
                location.path.clone(),
                documents::read(self.workspace.root(), &location.path).await?,
            );
        }
        let text = &self.texts[&location.path];
        let start = documents::convert(
            text,
            Position::new(location.line, location.column),
            "utf-16",
            "utf-32",
        )?;
        let end = documents::convert(
            text,
            Position::new(location.end_line, location.end_column),
            "utf-16",
            "utf-32",
        )?;
        Ok(
            json!({"path":self.workspace.relative(&location.path),"external":false,"line":u64::from(start.line)+1,"column":u64::from(start.character)+1,"end_line":u64::from(end.line)+1,"end_column":u64::from(end.character)+1,"column_encoding":"unicode"}),
        )
    }
}
// Keep both metadata and the reference page below the registry's spill threshold.
const REFERENCE_ENTRY_BYTES: usize = 6_000;
const REFERENCE_SUMMARY_BYTES: usize = 3_000;

#[derive(Clone, Copy)]
enum ResultFormat<'a> {
    Standard,
    References(Option<&'a Path>),
}

async fn render(
    workspace: &Workspace,
    result: QueryResult,
    limit: usize,
    offset: usize,
    format: ResultFormat<'_>,
) -> Result<Value, IndexError> {
    let mut locations = Locations {
        workspace,
        texts: BTreeMap::new(),
    };
    let mut entries = Vec::new();
    let mut truncated = result.truncated;
    let mut page = serde_json::Map::new();
    match result.data {
        QueryData::Symbols(items) => {
            let mut pending: Vec<_> = items
                .into_iter()
                .rev()
                .map(|s| (s, None::<String>))
                .collect();
            while let Some((symbol, container)) = pending.pop() {
                if entries.len() == limit {
                    truncated = true;
                    break;
                }
                entries.push(json!({"name":symbol.name,"kind":symbol.kind,"container":container,"location":locations.render(&symbol.location).await?}));
                pending.extend(
                    symbol
                        .children
                        .into_iter()
                        .rev()
                        .map(|s| (s, Some(symbol.name.clone()))),
                );
            }
        }
        QueryData::Locations(mut items) => {
            let bounded = matches!(format, ResultFormat::References(_));
            if let ResultFormat::References(Some(file)) = format {
                items.retain(|item| item.path == file);
            }
            let total = items.len();
            let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
            for item in &items {
                *file_counts
                    .entry(
                        workspace
                            .relative(&item.path)
                            .to_string_lossy()
                            .into_owned(),
                    )
                    .or_default() += 1;
            }
            items.sort_by(|a, b| {
                let a_local = workspace.relative(&a.path) != a.path;
                let b_local = workspace.relative(&b.path) != b.path;
                b_local
                    .cmp(&a_local)
                    .then(workspace.relative(&a.path).cmp(workspace.relative(&b.path)))
                    .then(a.line.cmp(&b.line))
                    .then(a.column.cmp(&b.column))
                    .then(a.end_line.cmp(&b.end_line))
                    .then(a.end_column.cmp(&b.end_column))
            });
            let mut bytes = 0;
            for item in items.into_iter().skip(offset).take(limit) {
                let entry = locations.render(&item).await?;
                bytes += entry.to_string().len() + 1;
                if bounded && bytes > REFERENCE_ENTRY_BYTES && !entries.is_empty() {
                    break;
                }
                entries.push(entry);
            }
            let next = offset.saturating_add(entries.len());
            truncated |= next < total;
            let file_count = file_counts.len();
            let mut summary = Vec::new();
            let mut bytes = 0;
            for (path, count) in file_counts {
                let entry = json!({"path":path,"count":count});
                bytes += entry.to_string().len() + 1;
                if bounded && bytes > REFERENCE_SUMMARY_BYTES {
                    break;
                }
                summary.push(entry);
            }
            page.insert("offset".into(), json!(offset));
            page.insert("total".into(), json!(total));
            page.insert("file_counts".into(), json!(summary));
            if bounded {
                page.insert("next_offset".into(), json!((next < total).then_some(next)));
                page.insert("file_count".into(), json!(file_count));
                page.insert(
                    "file_counts_truncated".into(),
                    json!(summary.len() < file_count),
                );
            }
        }
        QueryData::Calls(items) => {
            let mut budget = limit;
            for call in items {
                if budget == 0 {
                    truncated = true;
                    break;
                }
                budget -= 1;
                truncated |= call.sites.len() > budget;
                let mut sites = Vec::new();
                for site in call.sites.into_iter().take(budget) {
                    sites.push(locations.render(&site).await?);
                }
                budget -= sites.len();
                entries.push(json!({"name":call.name,"subject":locations.render(&call.subject).await?,"target":locations.render(&call.target).await?,"sites":sites}));
            }
        }
    }
    let mut value = json!({"status":"ok","source":result.source,"coverage":result.coverage,"generation":result.generation,"completeness_known":result.completeness_known,"truncated":truncated,"limit":limit,"position_base":1,"entries":entries});
    value.as_object_mut().unwrap().extend(page);
    Ok(value)
}

/// Point the model at the disambiguating tool when a search returns same-name symbols.
fn add_duplicate_hint(value: &mut Value) {
    let Some(entries) = value["entries"].as_array() else {
        return;
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        if let Some(name) = entry["name"].as_str() {
            *counts.entry(name.to_string()).or_default() += 1;
        }
    }
    let duplicates: Vec<_> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .take(3)
        .collect();
    if duplicates.is_empty() {
        return;
    }
    value["hint"] = json!(format!(
        "Several returned symbols share a name ({}). Before renaming or editing one, call \
         code_references on its declaration to confirm usages; a same-named \
         token may bind to another type, crate or import.",
        duplicates.join(", ")
    ));
}

/// Whether any project in this workspace can still answer semantic queries.
pub(crate) fn available(manager: &CodeIndexManager, workspace: &Path) -> bool {
    let Some(root) = canonical_root(workspace) else {
        return false;
    };
    match manager.status(&root) {
        Ok(status) => {
            status.discovering
                || status.projects.is_empty()
                || status.projects.iter().any(|project| {
                    !matches!(
                        project.state,
                        ServerState::Disabled | ServerState::Failed(_) | ServerState::Closed
                    )
                })
        }
        Err(_) => false,
    }
}

/// Tell the model which languages can answer before it decides to try.
fn index_note(manager: &CodeIndexManager, workspace: &Path) -> Option<String> {
    let root = canonical_root(workspace)?;
    let status = manager.status(&root).ok()?;
    if status.discovering {
        return Some(
            "Semantic index is starting for this workspace; code_* may return not_ready until it \
             finishes. Use read_file/grep meanwhile."
                .into(),
        );
    }
    if status.projects.is_empty() {
        return Some(
            "Semantic index found no supported project here; do not call code_* in this \
             workspace. Use read_file/grep."
                .into(),
        );
    }
    let mut ready = BTreeSet::new();
    let mut starting = BTreeSet::new();
    let mut unavailable = BTreeSet::new();
    for project in &status.projects {
        let name = language_name(project.language);
        match project.state {
            ServerState::Ready => {
                ready.insert(name);
            }
            ServerState::Idle | ServerState::Queued | ServerState::Starting => {
                starting.insert(name);
            }
            ServerState::Failed(_) | ServerState::Disabled | ServerState::Closed => {
                unavailable.insert(name);
            }
        }
    }
    let mut parts = Vec::new();
    let has_ready = !ready.is_empty();
    if !ready.is_empty() {
        parts.push(format!(
            "ready for {}",
            ready.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if !starting.is_empty() {
        parts.push(format!(
            "starting for {}",
            starting.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if !unavailable.is_empty() {
        parts.push(format!(
            "unavailable for {}",
            unavailable.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let action = if !has_ready {
        "code_* will return not_ready until a server is ready; use read_file/grep meanwhile."
    } else {
        "Call code_* before grep for symbol, reference and caller questions."
    };
    Some(format!("Semantic index {}. {action}", parts.join("; ")))
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::Rust => "Rust",
        Language::TypeScript => "TypeScript",
        Language::Python => "Python",
        Language::Go => "Go",
        Language::Java => "Java",
    }
}

fn canonical_root(workspace: &Path) -> Option<PathBuf> {
    Workspace::new(&workspace.to_string_lossy())
        .ok()
        .map(|workspace| workspace.root().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::{QueryCoverage, QuerySource, Symbol};
    #[test]
    fn validates_schema_arguments_and_failure_status() {
        let mut registry = ToolRegistry::new();
        register(&mut registry, Arc::new(CodeIndexManager::new()));
        assert!(registry.warnings().is_empty());
        assert_eq!(
            registry
                .guidance()
                .iter()
                .filter(|g| g.contains("code_symbols"))
                .count(),
            1
        );
        for (name, value) in [
            ("code_symbols", json!({"path":"a.rs","line":1})),
            ("code_symbols", json!({"path":"a.rs","query":" "})),
            ("code_symbols", json!({"path":"a.rs","limit":201})),
            ("code_symbols", json!({})),
            (
                "code_definition",
                json!({"path":"a.rs","line":0,"column":1}),
            ),
            (
                "code_references",
                json!({"path":"a.rs","line":1,"column":1,"direction":"incoming"}),
            ),
            ("code_symbols", json!({"path":"a.rs","offset":1})),
            (
                "code_references",
                json!({"path":"a.rs","line":1,"column":1,"offset":1000001}),
            ),
            ("code_calls", json!({"path":"a.rs","line":1,"column":1})),
        ] {
            assert!(validate(name, &parse_args::<Args>(value).unwrap()).is_err());
            assert_eq!(
                registry.get(name).unwrap().parameters()["additionalProperties"],
                false
            );
        }
        assert!(
            registry.get("code_symbols").unwrap().parameters()["required"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        for (name, value) in [
            ("code_symbols", json!({"query":"Data"})),
            ("code_symbols", json!({"path":"","query":"Data"})),
            (
                "code_definition",
                json!({"path":"a.rs","line":1,"column":1}),
            ),
            (
                "code_references",
                json!({"path":"a.rs","line":1,"column":1,"offset":200}),
            ),
        ] {
            assert!(validate(name, &parse_args::<Args>(value).unwrap()).is_ok());
        }
        for value in [
            json!({"path":"a.rs","extra":true}),
            json!({"path":"a.rs","line":1.5}),
            json!({"path":"a.rs","direction":"both"}),
            json!({"path":"a.rs","line":1,"column":1,"offset":-1}),
        ] {
            assert!(parse_args::<Args>(value).is_err());
        }
        for (error, status) in [
            (IndexError::Timeout, "timeout"),
            (IndexError::NotReady, "not_ready"),
            (IndexError::NotReadyReason("workspace_changed"), "not_ready"),
            (IndexError::NoReferences, "no_results"),
            (IndexError::Unsupported("calls".into()), "unsupported"),
            (IndexError::Backend("missing".into()), "unavailable"),
        ] {
            let ToolError::Failed(message) = failure(error) else {
                panic!()
            };
            let value: Value = serde_json::from_str(&message).unwrap();
            assert_eq!(value["status"], status);
            assert_eq!(value["completeness_known"], false);
        }
    }

    #[tokio::test]
    async fn reference_pages_keep_file_summary_without_spilling() {
        let (ctx, root, _owner) = super::super::test_support::context("reference-page");
        let source = root.join("a.rs");
        let caller = root.join("b.rs");
        std::fs::write(&source, "a\n".repeat(200)).unwrap();
        std::fs::write(&caller, "b\n").unwrap();
        let location = |path: PathBuf, line| Location {
            path,
            line,
            column: 0,
            end_line: line,
            end_column: 1,
        };
        let result = QueryResult {
            content_hash: None,
            source: QuerySource::Live,
            coverage: QueryCoverage::default(),
            generation: 5,
            truncated: false,
            completeness_known: false,
            data: QueryData::Locations(
                (0..200)
                    .map(|line| location(source.clone(), line))
                    .chain([location(caller.clone(), 0)])
                    .collect(),
            ),
        };
        let first = render(
            &ctx.workspace,
            result.clone(),
            200,
            0,
            ResultFormat::References(None),
        )
        .await
        .unwrap();
        assert!(first.to_string().chars().count() < super::super::TOOL_OUTPUT_THRESHOLD_CHARS);
        assert_eq!(first["file_counts"][1], json!({"path":"b.rs","count":1}));
        let next = first["next_offset"].as_u64().unwrap() as usize;
        assert_eq!(next, first["entries"].as_array().unwrap().len());
        let second = render(
            &ctx.workspace,
            result.clone(),
            200,
            next,
            ResultFormat::References(None),
        )
        .await
        .unwrap();
        assert_eq!(second["entries"][0]["line"], next + 1);
        let focused = render(
            &ctx.workspace,
            result,
            12,
            0,
            ResultFormat::References(Some(&caller)),
        )
        .await
        .unwrap();
        assert_eq!(focused["total"], 1);
        assert_eq!(focused["entries"][0]["path"], "b.rs");
        assert!(focused["next_offset"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn unicode_nested_limits_and_external_metadata() {
        let (ctx, root, _owner) = super::super::test_support::context("code-render");
        std::fs::write(root.join("a.rs"), "a😀中\r\n").unwrap();
        let location = Location {
            path: root.join("a.rs"),
            line: 0,
            column: 3,
            end_line: 0,
            end_column: 4,
        };
        let child = Symbol {
            name: "same".into(),
            kind: 12,
            location: location.clone(),
            body_lines: None,
            children: vec![],
        };
        let result = QueryResult {
            content_hash: None,
            source: QuerySource::Sqlite,
            coverage: QueryCoverage::default(),
            generation: 4,
            truncated: false,
            completeness_known: false,
            data: QueryData::Symbols(vec![Symbol {
                children: vec![child.clone()],
                ..child
            }]),
        };
        let limited = render(&ctx.workspace, result.clone(), 1, 0, ResultFormat::Standard)
            .await
            .unwrap();
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["entries"][0]["location"]["column"], 3);
        let full = render(&ctx.workspace, result, 50, 0, ResultFormat::Standard)
            .await
            .unwrap();
        assert_eq!(full["entries"][1]["container"], "same");
        assert_eq!(full["source"], "sqlite");
        assert_eq!(full["truncated"], false);
        let mut locations = Locations {
            workspace: &ctx.workspace,
            texts: BTreeMap::new(),
        };
        let external = Location {
            path: PathBuf::from("/outside/does-not-exist.rs"),
            ..location.clone()
        };
        let value = locations.render(&external).await.unwrap();
        assert_eq!(value["external"], true);
        assert_eq!(value["column_encoding"], "utf-16");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/outside/does-not-exist.rs", root.join("escape.rs"))
                .unwrap();
            let external = Location {
                path: root.join("escape.rs"),
                ..location
            };
            assert_eq!(locations.render(&external).await.unwrap()["external"], true);
        }
        assert!(locations.texts.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn location_pages_expose_all_files() {
        let (ctx, root, _owner) = super::super::test_support::context("code-page");
        std::fs::write(root.join("a.rs"), "aaaa\nbbbb\ncccc\n").unwrap();
        std::fs::write(root.join("b.rs"), "dddd\n").unwrap();
        let location = |name: &str, line: u32| Location {
            path: root.join(name),
            line,
            column: 0,
            end_line: line,
            end_column: 1,
        };
        let result = QueryResult {
            content_hash: None,
            source: QuerySource::Live,
            coverage: QueryCoverage::default(),
            generation: 5,
            truncated: false,
            completeness_known: false,
            data: QueryData::Locations(vec![
                location("a.rs", 0),
                location("a.rs", 1),
                location("a.rs", 2),
                location("b.rs", 0),
            ]),
        };

        let first = render(&ctx.workspace, result.clone(), 2, 0, ResultFormat::Standard)
            .await
            .unwrap();
        assert_eq!(first["offset"], 0);
        assert_eq!(first["total"], 4);
        assert_eq!(first["truncated"], true);
        assert_eq!(first["entries"].as_array().unwrap().len(), 2);
        assert_eq!(first["file_counts"][0]["path"], "a.rs");
        assert_eq!(first["file_counts"][0]["count"], 3);
        assert_eq!(first["file_counts"][1]["path"], "b.rs");
        assert_eq!(first["file_counts"][1]["count"], 1);

        let second = render(&ctx.workspace, result, 2, 2, ResultFormat::Standard)
            .await
            .unwrap();
        assert_eq!(second["offset"], 2);
        assert_eq!(second["total"], 4);
        assert_eq!(second["truncated"], false);
        let entries = second["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["path"], "a.rs");
        assert_eq!(entries[1]["path"], "b.rs");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_symbol_names_point_at_references() {
        let mut ambiguous = json!({"entries":[{"name":"Data"},{"name":"Data"},{"name":"Style"}]});
        add_duplicate_hint(&mut ambiguous);
        let hint = ambiguous["hint"].as_str().unwrap();
        assert!(hint.contains("Data"));
        assert!(hint.contains("code_references"));

        let mut unique = json!({"entries":[{"name":"Data"}]});
        add_duplicate_hint(&mut unique);
        assert!(unique.get("hint").is_none());
    }
}
