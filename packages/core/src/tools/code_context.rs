//! Small source excerpts for a symbol, text match or known position.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures_util::future::BoxFuture;
use lsp_types::Position;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolError, ToolOutput, parse_args, search};
use crate::code_index::{
    CodeIndexManager, IndexError, Location, Query, QueryData, ServerState, Symbol, documents,
};

const PAGE_FILES: usize = 4;
const SCAN_FILES: usize = 250;
const EXCERPT_LINES: usize = 18;
const EXCERPT_BYTES: usize = 1_500;
const FILES_BUDGET: usize = 8_000;
const REFERENCES_BUDGET: usize = 2_000;
const SEMANTIC_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) struct CodeContext(pub(crate) Arc<CodeIndexManager>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    query: Option<String>,
    path: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    include_references: Option<bool>,
    offset: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reason {
    Definition,
    Symbol,
    Requested,
    Reference,
    FileName,
    Text,
}

impl Reason {
    fn label(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Symbol => "symbol candidate",
            Self::Requested => "requested position",
            Self::Reference => "semantic reference",
            Self::Text => "text match",
            Self::FileName => "file name match",
        }
    }
}

#[derive(Default)]
struct Candidate {
    hits: Vec<(Reason, usize)>,
    body: Option<([u32; 2], blake3::Hash)>,
}

type Candidates = BTreeMap<PathBuf, Candidate>;

impl Tool for CodeContext {
    fn name(&self) -> &'static str {
        "code_context"
    }

    fn description(&self) -> &'static str {
        "Find relevant source snippets by symbol, literal error text, file name or known source position. Returns a small page of files with line numbers and why each matched. Optionally summarize references for an interface change."
    }

    fn parameters(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"properties":{
            "query":{"type":"string","minLength":1,"maxLength":512,"description":"Exact symbol name or literal text, such as an error message, route or config key."},
            "path":{"type":"string","minLength":1,"description":"Limit text search to this file or directory. Provide a file when using line or omitting query. Semantic navigation may follow a definition to another file."},
            "line":{"type":"integer","minimum":1,"description":"One-based line in path."},
            "column":{"type":"integer","minimum":1,"description":"One-based Unicode character column. With line, resolves the symbol at that position."},
            "include_references":{"type":"boolean","description":"For an interface change, summarize references to a uniquely resolved symbol and surface test candidates."},
            "offset":{"type":"integer","minimum":0,"description":"Use next_offset to see more candidate files on an unchanged workspace."}
        }})
    }

    fn guidance(&self) -> &'static str {
        "Use code_context for initial code navigation when you have a symbol, error text, or source position. Use path to focus on a candidate file, read_file for exact ranges, and grep for regex or broader text search."
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(40))
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            self.context(ctx, parse_args(args)?)
                .await
                .map(ToolOutput::text)
        })
    }
}

impl CodeContext {
    async fn context(&self, ctx: &ToolContext, args: Args) -> Result<String, ToolError> {
        let query = args.query.as_deref().map(str::trim);
        if query.is_some_and(|query| query.is_empty() || query.chars().count() > 512)
            || args.line == Some(0)
            || args.column == Some(0)
            || args.column.is_some() && args.line.is_none()
        {
            return Err(ToolError::InvalidArguments(
                "Use nonempty query text up to 512 characters and one-based line and column."
                    .into(),
            ));
        }
        let workspace = &ctx.workspace;
        let root = workspace.root();
        let path = workspace.resolve(args.path.as_deref().unwrap_or("."))?;
        if !path.exists() || (args.line.is_some() || query.is_none()) && !path.is_file() {
            return Err(ToolError::InvalidArguments(
                "Provide an existing file for a position or a file or directory with query.".into(),
            ));
        }

        let mut candidates = Candidates::new();
        let explicit = args.line.or_else(|| query.is_none().then_some(1));
        let mut position = None;
        if let Some(line) = explicit {
            let text = documents::read(root, &path)
                .await
                .map_err(|e| e.to_string())?;
            if text.contains('\0') {
                return Err(ToolError::InvalidArguments(
                    "Expected a UTF-8 text file.".into(),
                ));
            }
            let start = documents::convert(
                &text,
                Position::new(line - 1, args.column.unwrap_or(1) - 1),
                "utf-32",
                "utf-16",
            )
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
            if args.column.is_some() {
                position = Some((start, blake3::hash(text.as_bytes())));
            }
            candidates
                .entry(path.clone())
                .or_default()
                .hits
                .push((Reason::Requested, line as usize - 1));
        }

        let want_references = args.include_references.unwrap_or(false);
        let semantic = self.semantic(ctx, &path, query, position, want_references);
        let lexical = async {
            let Some(query) = query else {
                return Ok((Candidates::new(), false));
            };
            let workspace = workspace.clone();
            let cancel = ctx.cancel.clone();
            let path = path.clone();
            let query = query.to_owned();
            tokio::task::spawn_blocking(move || {
                let mut hits = Candidates::new();
                let capped = match search::scan_text_files(
                    &workspace,
                    &cancel,
                    &path,
                    None,
                    |relative, text| {
                        let mut matched: Vec<_> = text
                            .lines()
                            .enumerate()
                            .filter(|(_, line)| line.contains(&query))
                            .take(8)
                            .map(|(line, _)| (Reason::Text, line))
                            .collect();
                        if relative
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().contains(&query))
                        {
                            matched.push((Reason::FileName, 0));
                        }
                        if !matched.is_empty() {
                            if hits.len() == SCAN_FILES {
                                return false;
                            }
                            hits.entry(workspace.root().join(relative))
                                .or_default()
                                .hits = matched;
                        }
                        true
                    },
                ) {
                    Ok(capped) => capped,
                    Err(ToolError::Timeout(_)) => true,
                    Err(error) => return Err(error),
                };
                Ok((hits, capped))
            })
            .await
            .map_err(|e| ToolError::Failed(format!("Context search failed: {e}")))?
        };
        let ((semantic_status, semantic_hits, references), lexical) =
            tokio::join!(semantic, lexical);
        let (text_hits, scan_capped) = lexical?;
        for (path, found) in text_hits {
            candidates.entry(path).or_default().hits.extend(found.hits);
        }
        for (path, found) in semantic_hits {
            let target = candidates.entry(path).or_default();
            target.hits.extend(found.hits);
            target.body = found.body;
        }
        let (files, count, next_offset) =
            render(ctx, candidates, args.offset.unwrap_or(0), want_references).await;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(json!({
            "files": files, "candidate_files": count, "next_offset": next_offset,
            "text_scan_capped": scan_capped, "semantic": semantic_status, "references": references
        })
        .to_string())
    }

    async fn query(
        &self,
        ctx: &ToolContext,
        query: Query,
    ) -> Result<crate::code_index::QueryResult, IndexError> {
        tokio::time::timeout(
            SEMANTIC_TIMEOUT,
            self.0.query(ctx.workspace.root(), query, &ctx.cancel),
        )
        .await
        .unwrap_or(Err(IndexError::Timeout))
    }

    async fn semantic(
        &self,
        ctx: &ToolContext,
        path: &Path,
        query: Option<&str>,
        position: Option<(Position, blake3::Hash)>,
        want_references: bool,
    ) -> (&'static str, Candidates, Value) {
        let mut hits = Candidates::new();
        let (reason, lookup) = if let Some((position, _)) = position {
            (
                Reason::Definition,
                Query::Definition {
                    file: path.to_string_lossy().into_owned(),
                    line: position.line,
                    column: position.character,
                },
            )
        } else if let Some(name) =
            query.filter(|text| text.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
        {
            (
                Reason::Symbol,
                Query::WorkspaceSymbols {
                    file: path.is_file().then(|| path.to_string_lossy().into_owned()),
                    query: name.into(),
                },
            )
        } else {
            return ("not_requested", hits, Value::Null);
        };
        if !self.0.status(ctx.workspace.root()).is_ok_and(|status| {
            status
                .projects
                .iter()
                .any(|project| project.state == ServerState::Ready)
        }) {
            return ("unavailable", hits, Value::Null);
        }
        let result = match self.query(ctx, lookup).await {
            Ok(result) => result,
            Err(IndexError::NotReady | IndexError::NotReadyReason(_)) => {
                return ("not_ready", hits, Value::Null);
            }
            Err(IndexError::Timeout) => return ("timed_out", hits, Value::Null),
            Err(_) => return ("unavailable", hits, Value::Null),
        };
        if let Some((_, hash)) = position
            && (result.content_hash.as_deref() != Some(hash.to_hex().as_str())
                || !result.coverage.file_verified)
        {
            return ("not_ready", hits, Value::Null);
        }
        let mut locations = match result.data {
            QueryData::Locations(locations) => locations,
            QueryData::Symbols(symbols) => symbols
                .into_iter()
                .filter(|symbol| {
                    Some(symbol.name.as_str()) == query && symbol.location.path.starts_with(path)
                })
                .map(|symbol| symbol.location)
                .collect(),
            QueryData::Calls(_) => return ("unavailable", hits, Value::Null),
        };
        locations
            .sort_unstable_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
        locations.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.column == b.column);
        for location in &locations {
            if let Some(path) = local_path(ctx, location) {
                hits.entry(path)
                    .or_default()
                    .hits
                    .push((reason, location.line as usize));
            }
        }
        let mut impact = if want_references {
            json!({"status":if locations.len() > 1 {"ambiguous"} else {"unavailable"}})
        } else {
            Value::Null
        };
        if let [location] = locations.as_slice()
            && let Some(path) = local_path(ctx, location)
        {
            let name = if reason == Reason::Symbol {
                query
            } else {
                None
            };
            let symbol = self.document_symbol(ctx, &path, location, name).await;
            if want_references {
                let target = symbol
                    .as_ref()
                    .map_or(location, |(symbol, _)| &symbol.location);
                impact = self.references(ctx, &path, target, &mut hits).await;
            }
            hits.entry(path).or_default().body =
                symbol.and_then(|(symbol, hash)| symbol.body_lines.map(|lines| (lines, hash)));
        }
        (
            if locations.is_empty() {
                "no_exact_match"
            } else {
                "answered"
            },
            hits,
            impact,
        )
    }

    async fn document_symbol(
        &self,
        ctx: &ToolContext,
        path: &Path,
        location: &Location,
        name: Option<&str>,
    ) -> Option<(Symbol, blake3::Hash)> {
        let root = ctx.workspace.root();
        let before = documents::read(root, path).await.ok()?;
        let query = Query::Symbols {
            file: path.to_string_lossy().into_owned(),
        };
        let result = self.query(ctx, query).await.ok()?;
        let QueryData::Symbols(symbols) = result.data else {
            return None;
        };
        if !result.coverage.file_verified || documents::read(root, path).await.ok()? != before {
            return None;
        }
        let mut pending = symbols;
        let mut same_name = Vec::new();
        let hash = blake3::hash(before.as_bytes());
        while let Some(mut symbol) = pending.pop() {
            pending.append(&mut symbol.children);
            if name.is_some_and(|name| name != symbol.name) {
                continue;
            }
            if symbol.location.line == location.line && symbol.location.column == location.column {
                return Some((symbol, hash));
            }
            if name.is_some()
                && symbol
                    .body_lines
                    .is_some_and(|[start, end]| (start..=end).contains(&location.line))
            {
                same_name.push(symbol);
            }
        }
        (same_name.len() == 1).then(|| (same_name.pop().unwrap(), hash))
    }

    async fn references(
        &self,
        ctx: &ToolContext,
        path: &Path,
        location: &Location,
        hits: &mut Candidates,
    ) -> Value {
        let query = Query::References {
            file: path.to_string_lossy().into_owned(),
            line: location.line,
            column: location.column,
        };
        let result = match self.query(ctx, query).await {
            Ok(result) => result,
            Err(IndexError::NotReadyReason(reason)) => {
                return json!({"status":"not_ready","reason":reason});
            }
            Err(IndexError::NoReferences) => {
                return json!({"status":"no_results","completeness_known":false});
            }
            Err(IndexError::NotReady | IndexError::Busy) => return json!({"status":"not_ready"}),
            Err(IndexError::Timeout) => return json!({"status":"timed_out"}),
            Err(_) => return json!({"status":"unavailable"}),
        };
        let QueryData::Locations(locations) = result.data else {
            return json!({"status":"unavailable"});
        };
        let mut by_file = BTreeMap::<PathBuf, BTreeSet<(u32, u32)>>::new();
        let mut resolved = BTreeMap::new();
        let mut external = 0;
        for location in &locations {
            if let Some(path) = resolved
                .entry(&location.path)
                .or_insert_with(|| local_path(ctx, location))
            {
                by_file
                    .entry(path.clone())
                    .or_default()
                    .insert((location.line, location.column));
            } else {
                external += 1;
            }
        }
        for (path, positions) in &by_file {
            hits.entry(path.clone()).or_default().hits.extend(
                positions
                    .iter()
                    .take(8)
                    .map(|(line, _)| (Reason::Reference, *line as usize)),
            );
        }
        let mut summary = Vec::new();
        let mut bytes = 0;
        for (path, positions) in by_file.iter().take(12) {
            let entry = json!([
                search::display_path(ctx.workspace.relative(path)),
                positions.len()
            ]);
            bytes += entry.to_string().len() + 1;
            if bytes > REFERENCES_BUDGET {
                break;
            }
            summary.push(entry);
        }
        json!({
            "status":"answered", "file_counts":summary, "workspace_files":by_file.len(),
            "external_locations":external, "completeness_known":result.completeness_known,
            "summary_truncated":result.truncated || summary.len() < by_file.len()
        })
    }
}

fn local_path(ctx: &ToolContext, location: &Location) -> Option<PathBuf> {
    let path = ctx.workspace.resolve(location.path.to_str()?).ok()?;
    path.is_file().then_some(path)
}

// shortcut: test candidates use common file names and directories, add language-specific detection when needed.
fn test_candidate(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name.starts_with("test_")
        || [".test.", ".spec.", "_test."]
            .iter()
            .any(|marker| name.contains(marker))
        || path
            .iter()
            .any(|part| matches!(part.to_str(), Some("tests" | "test" | "__tests__")))
}

async fn render(
    ctx: &ToolContext,
    candidates: Candidates,
    offset: usize,
    prefer_tests: bool,
) -> (Vec<Value>, usize, Option<usize>) {
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    for (_, candidate) in &mut candidates {
        candidate.hits.sort_unstable();
        candidate.hits.dedup();
    }
    candidates.sort_by_key(|(path, candidate)| {
        let first = candidate.hits[0].0;
        let related = first >= Reason::Reference;
        (
            related,
            related && prefer_tests && !test_candidate(ctx.workspace.relative(path)),
            first,
        )
    });
    let count = candidates.len();
    let mut files = Vec::new();
    let mut bytes = 0;
    for (path, candidate) in candidates.into_iter().skip(offset).take(PAGE_FILES) {
        if ctx.cancel.is_cancelled() {
            break;
        }
        let relative = ctx.workspace.relative(&path);
        let mut file = json!({
            "path":search::display_path(relative), "test_candidate":test_candidate(relative),
            "matches":candidate.hits.iter().take(8).map(|(reason, line)| json!([reason.label(), line+1])).collect::<Vec<_>>(),
        });
        if let Ok(text) = documents::read(ctx.workspace.root(), &path).await
            && !text.contains('\0')
        {
            let lines: Vec<_> = text.split('\n').collect();
            let mut selected = BTreeSet::new();
            if let Some(([start, end], hash)) = candidate.body
                && blake3::hash(text.as_bytes()) == hash
            {
                let end = end as usize;
                let start = start as usize;
                selected.extend(
                    (start..=end)
                        .take(EXCERPT_LINES)
                        .filter(|line| *line < lines.len()),
                );
                file["symbol_lines"] = json!([start + 1, end + 1]);
            }
            for (_, line) in &candidate.hits {
                if selected.len() == EXCERPT_LINES {
                    break;
                }
                if *line < lines.len() {
                    selected.insert(*line);
                }
            }
            for (_, line) in &candidate.hits {
                for nearby in line.saturating_sub(2)..line.saturating_add(5).min(lines.len()) {
                    if selected.len() == EXCERPT_LINES {
                        break;
                    }
                    selected.insert(nearby);
                }
            }
            let mut excerpt = String::new();
            for line in selected {
                let source = lines[line].trim_end_matches('\r');
                let remaining = EXCERPT_BYTES.saturating_sub(excerpt.len() + 16);
                if remaining == 0 {
                    break;
                }
                let end = source.floor_char_boundary(source.len().min(160).min(remaining));
                let suffix = if end < source.len() { "…" } else { "" };
                use std::fmt::Write;
                let _ = writeln!(excerpt, "{}: {}{suffix}", line + 1, &source[..end]);
            }
            file["excerpt"] = json!(excerpt);
        } else {
            file["excerpt_status"] = json!("unavailable");
        }
        bytes += file.to_string().len() + 1;
        if bytes > FILES_BUDGET && !files.is_empty() {
            break;
        }
        files.push(file);
    }
    let next = offset.saturating_add(files.len());
    (
        files,
        count,
        (next > offset && next < count).then_some(next),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn literal_search_merges_snippets_and_pages_files_without_lsp() {
        let (ctx, root, _owner) = super::super::test_support::context("code-context");
        for idx in 0..6 {
            std::fs::write(
                root.join(format!("module-{idx}.rs")),
                "before\nerror [E1]\nshared\nerror [E1]\nafter\n",
            )
            .unwrap();
        }
        std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(root.join("ignored.rs"), "error [E1]").unwrap();
        let tool = CodeContext(Arc::new(CodeIndexManager::new()));
        let first = tool
            .execute(&ctx, json!({"query":"error [E1]"}))
            .await
            .unwrap();
        let first: Value = serde_json::from_str(&first.text).unwrap();
        assert_eq!(first["candidate_files"], 6);
        assert_eq!(first["next_offset"], 4);
        assert_eq!(
            first["files"][0]["matches"],
            json!([["text match", 2], ["text match", 4]])
        );
        let excerpt = first["files"][0]["excerpt"].as_str().unwrap();
        assert_eq!(excerpt.matches("3: shared").count(), 1);
        let last = tool
            .execute(&ctx, json!({"query":"error [E1]","offset":4}))
            .await
            .unwrap();
        let last: Value = serde_json::from_str(&last.text).unwrap();
        assert_eq!(last["files"][0]["path"], "module-4.rs");
        assert_eq!(last["files"].as_array().unwrap().len(), 2);
        assert!(last["next_offset"].is_null());

        let direct = tool
            .execute(&ctx, json!({"path":"module-1.rs","line":3,"column":1}))
            .await
            .unwrap();
        let direct: Value = serde_json::from_str(&direct.text).unwrap();
        assert_eq!(direct["semantic"], "unavailable");
        assert_eq!(
            direct["files"][0]["matches"],
            json!([["requested position", 3]])
        );
        assert!(
            tool.execute(&ctx, json!({"path":"../outside","line":1}))
                .await
                .is_err()
        );
        assert!(
            tool.execute(&ctx, json!({"path":"module-1.rs","line":100}))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
