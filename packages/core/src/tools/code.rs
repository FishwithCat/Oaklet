//! Model positions are one-based Unicode characters; service positions are UTF-16.
use super::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, Workspace, parse_args};
use crate::code_index::{
    CodeIndexManager, IndexError, Location, Query, QueryData, QueryResult, documents,
};
use futures_util::future::BoxFuture;
use lsp_types::Position;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

const GUIDANCE: &str = "Use code_symbols, code_definition, code_references and code_calls for saved-source semantic navigation. Paths belong to this session's workspace. Lines and columns start at 1; columns count Unicode characters. code_calls returns one hop and preserves prepared subjects and same-name candidates; only server call hierarchy results are call evidence. Results may be partial even without truncation; cached symbols and protocol readiness do not prove semantic completeness. On unavailable, unsupported, not_ready or timeout, continue with grep/read_file and label text matches as candidates. External targets are metadata only; their columns remain explicitly UTF-16. Narrow truncated queries or raise limit up to 200. Oversized output is saved by the tool pipeline; use read_file with offset/limit on its spill path to recover all returned entries.";

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
    path: String,
    query: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    direction: Option<Direction>,
    limit: Option<usize>,
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
                "List saved-file symbols, or search the path's project with query. Returns distinct candidates with source and coverage; completeness is unknown."
            }
            "code_definition" => {
                "Locate definitions of a saved-source position. Multiple candidates are preserved. External targets are reported without reading them."
            }
            "code_references" => {
                "Locate references of a saved-source position, excluding the declaration. Partial results do not establish absence of references."
            }
            _ => {
                "Find one hop of incoming callers or outgoing callees using server call hierarchy. Preserves recursive calls, prepared subjects and distinct candidates; unsupported is not an empty call graph."
            }
        }
    }
    fn parameters(&self) -> Value {
        let mut properties = json!({
            "path": {"type":"string", "minLength":1, "description":"Saved source file in this session's workspace; selects the project for symbol search."},
            "limit": {"type":"integer", "minimum":1, "maximum":200, "default":50, "description":"Maximum entries; call sites share this budget."}
        });
        let mut required = vec!["path"];
        if self.name == "code_symbols" {
            properties["query"] = json!({"type":"string", "minLength":1, "maxLength":1024, "description":"Optional project symbol search (server matching rules). Omit to list file symbols."});
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
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn guidance(&self) -> &str {
        GUIDANCE
    }
    fn summary(&self, args: &Value) -> String {
        let path = args["path"].as_str().unwrap_or_default();
        match args.get("line").and_then(Value::as_u64) {
            Some(line) => format!(
                "{path}:{line}:{} {}",
                args["column"],
                args["direction"].as_str().unwrap_or_default()
            )
            .trim()
            .into(),
            None => format!("{path} {}", args["query"].as_str().unwrap_or_default())
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
            let limit = args.limit.unwrap_or(50);
            let work = async {
                let path = ctx
                    .workspace
                    .resolve(&args.path)
                    .map_err(IndexError::Invalid)?;
                let file = path.to_string_lossy().into_owned();
                let mut input_hash = None;
                let query = if self.name == "code_symbols" {
                    match args.query {
                        Some(query) => Query::WorkspaceSymbols { file, query },
                        None => Query::Symbols { file },
                    }
                } else {
                    let text = documents::read(ctx.workspace.root(), &path).await?;
                    input_hash = Some(blake3::hash(text.as_bytes()).to_hex().to_string());
                    let position = documents::convert(
                        &text,
                        Position::new(args.line.unwrap() - 1, args.column.unwrap() - 1),
                        "utf-32",
                        "utf-16",
                    )?;
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
                    .query(ctx.workspace.root(), query, &ctx.cancel)
                    .await?;
                if let Some(hash) = input_hash {
                    let current = documents::read(ctx.workspace.root(), &path).await?;
                    if result.content_hash.as_ref() != Some(&hash)
                        || blake3::hash(current.as_bytes()).to_hex().as_str() != hash
                    {
                        return Err(IndexError::NotReady);
                    }
                }
                render(&ctx.workspace, result, limit).await
            };
            let result = tokio::select! { biased;
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                result = work => result,
            };
            match result {
                Ok(value) => Ok(ToolOutput::text(
                    serde_json::to_string_pretty(&value)
                        .map_err(|e| ToolError::Failed(e.to_string()))?,
                )),
                Err(IndexError::Cancelled) => Err(ToolError::Cancelled),
                Err(error) => Err(failure(error)),
            }
        })
    }
}
fn validate(name: &str, args: &Args) -> Result<(), ToolError> {
    let invalid = if args.path.trim().is_empty() {
        Some("path must not be empty")
    } else if !(1..=200).contains(&args.limit.unwrap_or(50)) {
        Some("limit must be 1..200")
    } else if name == "code_symbols" {
        if args.line.is_some() || args.column.is_some() || args.direction.is_some() {
            Some("symbol queries do not accept positions or direction")
        } else if args
            .query
            .as_ref()
            .is_some_and(|q| q.trim().is_empty() || q.chars().count() > 1024)
        {
            Some("query must contain 1..1024 characters")
        } else {
            None
        }
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
        IndexError::Busy | IndexError::NotReady => "not_ready",
        IndexError::Closed | IndexError::Disabled | IndexError::Backend(_) => "unavailable",
        IndexError::Unsupported(_) => "unsupported",
        IndexError::Timeout => "timeout",
        IndexError::Invalid(_) => "invalid_arguments",
        IndexError::Cancelled => "cancelled",
    };
    ToolError::Failed(json!({"status":status,"detail":error.to_string(),"completeness_known":false,"guidance":"Use grep/read_file as fallback; text matches are candidates, not confirmed calls. No result here proves absence."}).to_string())
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
async fn render(
    workspace: &Workspace,
    result: QueryResult,
    limit: usize,
) -> Result<Value, IndexError> {
    let mut locations = Locations {
        workspace,
        texts: BTreeMap::new(),
    };
    let mut entries = Vec::new();
    let mut truncated = result.truncated;
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
        QueryData::Locations(items) => {
            truncated |= items.len() > limit;
            for item in items.into_iter().take(limit) {
                entries.push(locations.render(&item).await?);
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
    Ok(
        json!({"status":"ok","source":result.source,"coverage":result.coverage,"generation":result.generation,"completeness_known":result.completeness_known,"truncated":truncated,"limit":limit,"position_base":1,"entries":entries}),
    )
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
            (
                "code_definition",
                json!({"path":"a.rs","line":0,"column":1}),
            ),
            (
                "code_references",
                json!({"path":"a.rs","line":1,"column":1,"direction":"incoming"}),
            ),
            ("code_calls", json!({"path":"a.rs","line":1,"column":1})),
        ] {
            assert!(validate(name, &parse_args::<Args>(value).unwrap()).is_err());
            assert_eq!(
                registry.get(name).unwrap().parameters()["additionalProperties"],
                false
            );
        }
        for value in [
            json!({"path":"a.rs","extra":true}),
            json!({"path":"a.rs","line":1.5}),
            json!({"path":"a.rs","direction":"both"}),
        ] {
            assert!(parse_args::<Args>(value).is_err());
        }
        for (error, status) in [
            (IndexError::Timeout, "timeout"),
            (IndexError::NotReady, "not_ready"),
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
        let limited = render(&ctx.workspace, result.clone(), 1).await.unwrap();
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["entries"][0]["location"]["column"], 3);
        let full = render(&ctx.workspace, result, 50).await.unwrap();
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
}
