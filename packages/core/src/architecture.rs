//! Workspace architecture discovery and data-only LikeC4 preview loading.
//!
//! Discovery walks the selected workspace with the same ignore rules as the
//! repository. Loading reads the model sources, hands them to a pinned Node
//! helper over stdin, and returns the layouted JSON the React renderer needs.
//! The helper never reads the workspace or executes workspace configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::AppHandle;

use crate::tools::Workspace;

mod compiler;
mod discovery;
mod runtime;

use compiler::run_helper;
use discovery::{digest_sources, discover_models, normalize_root, read_sources};
use runtime::{runtime_status, runtime_status_for};

pub(crate) use compiler::read_capped;
pub use discovery::prompt_model_sources;
pub(crate) use runtime::{find_helper, find_node};

pub const DEFAULT_MODEL_PATH: &str = "architecture/model.c4";

const MAX_DIAGNOSTICS: usize = 50;
const MAX_DIAGNOSTIC_CHARS: usize = 2_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub ready: bool,
    pub node: Option<String>,
    pub node_version: Option<String>,
    pub helper: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoot {
    pub root: String,
    pub sources: Vec<String>,
    pub config: Option<String>,
    pub unsupported_config: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub workspace: String,
    pub models: Vec<ModelRoot>,
    pub default_path: String,
    pub runtime: RuntimeStatus,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewInfo {
    pub id: String,
    pub title: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadResult {
    pub workspace: String,
    pub root: String,
    pub sources: Vec<SourceInfo>,
    pub views: Vec<ViewInfo>,
    pub model: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
    pub stale: bool,
    pub cached: bool,
    pub source_digest: String,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct CachedModel {
    workspace: String,
    root: String,
    digest: String,
    model: Value,
    views: Vec<ViewInfo>,
}

struct LoadBase {
    workspace: String,
    root: String,
    sources: Vec<SourceInfo>,
    digest: String,
    warnings: Vec<String>,
}

#[derive(Default)]
pub struct ArchitectureCache {
    cached: Option<CachedModel>,
    cancel: Option<Arc<AtomicBool>>,
    generation: u64,
}

/// One architecture load attempt. Creating it supersedes any older attempt and
/// dropping it releases the cancellation slot when this attempt still owns it.
struct LoadTicket {
    shared: Arc<crate::AppState>,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl LoadTicket {
    fn new(shared: Arc<crate::AppState>) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = {
            let mut cache = shared
                .architecture
                .lock()
                .map_err(|_| "Architecture state is unavailable.".to_string())?;
            if let Some(previous) = cache.cancel.take() {
                previous.store(true, Ordering::SeqCst);
            }
            cache.generation = cache.generation.wrapping_add(1);
            cache.cancel = Some(cancel.clone());
            cache.generation
        };
        Ok(Self {
            shared,
            generation,
            cancel,
        })
    }

    fn require_current(&self) -> Result<(), String> {
        let cache = self
            .shared
            .architecture
            .lock()
            .map_err(|_| "Architecture state is unavailable.".to_string())?;
        if cache.generation == self.generation {
            Ok(())
        } else {
            Err("Architecture load was superseded by a newer request.".into())
        }
    }
}

impl Drop for LoadTicket {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.shared.architecture.lock()
            && cache.generation == self.generation
        {
            cache.cancel = None;
        }
    }
}

/// Drop a model from another workspace and stop a compiler process that is no
/// longer relevant.
pub fn cancel_for_workspace_change(state: &crate::AppState) {
    if let Ok(mut cache) = state.architecture.lock() {
        cache.generation = cache.generation.wrapping_add(1);
        if let Some(cancel) = cache.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        cache.cached = None;
    }
}

/// Stop any compiler process when the application exits.
pub fn cancel_all(state: &crate::AppState) {
    if let Ok(mut cache) = state.architecture.lock() {
        cache.generation = cache.generation.wrapping_add(1);
        if let Some(cancel) = cache.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

pub fn discover(
    app: &AppHandle,
    workspace_path: &Path,
    include_ignored: bool,
) -> Result<Discovery, String> {
    let workspace = Workspace::new(&workspace_path.to_string_lossy())?;
    let (models, warnings) = discover_models(&workspace, include_ignored)?;
    Ok(Discovery {
        workspace: workspace.root().to_string_lossy().into_owned(),
        models,
        default_path: DEFAULT_MODEL_PATH.into(),
        runtime: runtime_status(app),
        warnings,
    })
}

pub async fn load(
    app: AppHandle,
    shared: Arc<crate::AppState>,
    workspace_path: PathBuf,
    requested_root: String,
    include_ignored: bool,
) -> Result<LoadResult, String> {
    let workspace = Workspace::new(&workspace_path.to_string_lossy())?;
    let workspace_text = workspace.root().to_string_lossy().into_owned();
    let root = normalize_root(&requested_root);
    let ticket = LoadTicket::new(shared.clone())?;

    let workspace_for_scan = workspace.clone();
    let (models, warnings) = tauri::async_runtime::spawn_blocking(move || {
        discover_models(&workspace_for_scan, include_ignored)
    })
    .await
    .map_err(|error| format!("Could not scan the workspace: {error}"))??;

    let Some(model) = models.into_iter().find(|model| model.root == root) else {
        return Err(format!(
            "The architecture model root was not found: {requested_root}"
        ));
    };

    let workspace_for_read = workspace.clone();
    let model_for_read = model.clone();
    let (source_map, sources) = tauri::async_runtime::spawn_blocking(move || {
        read_sources(&workspace_for_read, &model_for_read)
    })
    .await
    .map_err(|error| format!("Could not read the architecture model: {error}"))??;
    let digest = digest_sources(&source_map);
    ticket.require_current()?;

    let cached_match = {
        let cache = shared
            .architecture
            .lock()
            .map_err(|_| "Architecture state is unavailable.".to_string())?;
        cached_for(&cache, &workspace_text, &model.root)
    };

    if let Some(cached) = cached_match.as_ref()
        && cached.digest == digest
    {
        return Ok(LoadResult {
            workspace: workspace_text,
            root: model.root,
            sources,
            views: cached.views.clone(),
            model: Some(cached.model.clone()),
            diagnostics: Vec::new(),
            stale: false,
            cached: true,
            source_digest: digest,
            error: None,
            warnings,
        });
    }

    let base = LoadBase {
        workspace: workspace_text,
        root: model.root,
        sources,
        digest,
        warnings,
    };

    let node = find_node();
    let helper = find_helper(&app);
    let runtime = runtime_status_for(node.as_deref(), helper.as_deref());
    if !runtime.ready {
        return Ok(fallback_result(
            base,
            cached_match,
            Vec::new(),
            Some(format!(
                "LikeC4 preview is unavailable. {}",
                runtime.message
            )),
        ));
    }

    let Some(node) = node else {
        return Ok(fallback_result(
            base,
            cached_match,
            Vec::new(),
            Some("Node.js was not found. Install Node.js 22.22.3 or newer.".into()),
        ));
    };
    let Some(helper) = helper else {
        return Ok(fallback_result(
            base,
            cached_match,
            Vec::new(),
            Some(
                "The LikeC4 compiler helper is missing. Reinstall Oaklet or set OAKLET_ARCHITECTURE_HELPER."
                    .into(),
            ),
        ));
    };

    let current_workspace = shared
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .workspace
        .clone();
    if current_workspace != base.workspace {
        return Err("The workspace changed while architecture was loading.".into());
    }

    let request = serde_json::json!({
        "version": 1,
        "sources": source_map,
    });
    let outcome = run_helper(&node, &helper, &request, ticket.cancel.clone()).await;

    let current_workspace = shared
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .workspace
        .clone();
    if current_workspace != base.workspace {
        return Err("The workspace changed while architecture was loading.".into());
    }

    let mut cache = shared
        .architecture
        .lock()
        .map_err(|_| "Architecture state is unavailable.".to_string())?;
    if cache.generation != ticket.generation {
        return Err("Architecture load was superseded by a newer request.".into());
    }

    match outcome {
        Ok(response) if response.ok => {
            let Some(model_data) = response.model else {
                return Ok(fallback_result(
                    base,
                    cached_match,
                    cap_diagnostics(response.diagnostics),
                    Some("LikeC4 did not produce a layouted model.".into()),
                ));
            };
            if model_data.get("_stage").and_then(Value::as_str) != Some("layouted") {
                return Ok(fallback_result(
                    base,
                    cached_match,
                    cap_diagnostics(response.diagnostics),
                    Some("LikeC4 returned a model that was not layouted.".into()),
                ));
            }

            cache.cached = Some(CachedModel {
                workspace: base.workspace.clone(),
                root: base.root.clone(),
                digest: base.digest.clone(),
                model: model_data.clone(),
                views: response.views.clone(),
            });
            Ok(LoadResult {
                workspace: base.workspace,
                root: base.root,
                sources: base.sources,
                views: response.views,
                model: Some(model_data),
                diagnostics: Vec::new(),
                stale: false,
                cached: false,
                source_digest: base.digest,
                error: None,
                warnings: base.warnings,
            })
        }
        Ok(response) => Ok(fallback_result(
            base,
            cached_match,
            cap_diagnostics(response.diagnostics),
            Some(
                response
                    .error
                    .unwrap_or_else(|| "LikeC4 reported errors in the architecture model.".into()),
            ),
        )),
        Err(error) => Ok(fallback_result(base, cached_match, Vec::new(), Some(error))),
    }
}

fn fallback_result(
    base: LoadBase,
    cached: Option<CachedModel>,
    diagnostics: Vec<Diagnostic>,
    error: Option<String>,
) -> LoadResult {
    let (model, views) = match cached {
        Some(cached) => (Some(cached.model), cached.views),
        None => (None, Vec::new()),
    };
    let stale = model.is_some();
    LoadResult {
        workspace: base.workspace,
        root: base.root,
        sources: base.sources,
        views,
        model,
        diagnostics,
        stale,
        cached: false,
        source_digest: base.digest,
        error,
        warnings: base.warnings,
    }
}

fn cached_for(cache: &ArchitectureCache, workspace: &str, root: &str) -> Option<CachedModel> {
    cache
        .cached
        .as_ref()
        .filter(|cached| cached.workspace == workspace && cached.root == root)
        .cloned()
}

fn cap_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .take(MAX_DIAGNOSTICS)
        .map(|mut diagnostic| {
            diagnostic.message = crate::tools::cap(diagnostic.message, MAX_DIAGNOSTIC_CHARS);
            diagnostic
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::runtime::helper_ready;
    use super::*;

    fn cached_model(workspace: &str, root: &str) -> CachedModel {
        CachedModel {
            workspace: workspace.into(),
            root: root.into(),
            digest: "digest".into(),
            model: serde_json::json!({ "_stage": "layouted", "views": {} }),
            views: vec![ViewInfo {
                id: "index".into(),
                title: "System context".into(),
                kind: "element".into(),
            }],
        }
    }

    #[test]
    fn cache_only_matches_the_same_workspace_and_model_root() {
        let cache = ArchitectureCache {
            cached: Some(cached_model("/workspace", "architecture")),
            ..Default::default()
        };

        assert!(cached_for(&cache, "/workspace", "architecture").is_some());
        assert!(cached_for(&cache, "/workspace", "docs").is_none());
        assert!(cached_for(&cache, "/other", "architecture").is_none());
    }

    #[test]
    fn validation_failures_keep_only_a_same_model_diagram_and_mark_it_stale() {
        let base = LoadBase {
            workspace: "/workspace".into(),
            root: "architecture".into(),
            sources: Vec::new(),
            digest: "digest".into(),
            warnings: Vec::new(),
        };
        let diagnostics = vec![Diagnostic {
            message: "Target not resolved".into(),
            file: Some("architecture/model.c4".into()),
            line: Some(7),
            column: Some(3),
        }];

        let stale = fallback_result(
            base,
            Some(cached_model("/workspace", "architecture")),
            diagnostics,
            Some("The LikeC4 model has errors.".into()),
        );
        assert!(stale.stale);
        assert!(stale.model.is_some());
        assert_eq!(stale.views[0].id, "index");
        assert_eq!(stale.diagnostics.len(), 1);

        let empty = fallback_result(
            LoadBase {
                workspace: "/workspace".into(),
                root: "architecture".into(),
                sources: Vec::new(),
                digest: "digest".into(),
                warnings: Vec::new(),
            },
            None,
            Vec::new(),
            Some("The LikeC4 compiler failed.".into()),
        );
        assert!(!empty.stale);
        assert!(empty.model.is_none());
    }

    fn test_state() -> Arc<crate::AppState> {
        Arc::new(crate::AppState {
            config: std::sync::Mutex::new(crate::AppConfig::default()),
            config_path: PathBuf::new(),
            approvals: std::sync::Mutex::new(std::collections::HashMap::new()),
            chat: crate::chat::ChatState::in_memory(),
            command_allow_all: std::sync::Mutex::new(None),
            terminal: std::sync::Mutex::new(std::collections::HashMap::new()),
            architecture: std::sync::Mutex::new(ArchitectureCache::default()),
            ids: std::sync::atomic::AtomicU64::new(0),
        })
    }

    #[test]
    fn a_newer_ticket_supersedes_and_cancels_the_previous_one() {
        let shared = test_state();
        let first = LoadTicket::new(shared.clone()).unwrap();
        let first_cancel = first.cancel.clone();

        let second = LoadTicket::new(shared.clone()).unwrap();

        assert!(first_cancel.load(Ordering::SeqCst));
        assert!(first.require_current().unwrap_err().contains("superseded"));
        second.require_current().unwrap();
        assert_eq!(second.generation, first.generation + 1);
    }

    #[test]
    fn dropping_an_old_ticket_keeps_the_newer_cancellation_slot() {
        let shared = test_state();
        let first = LoadTicket::new(shared.clone()).unwrap();
        let second = LoadTicket::new(shared.clone()).unwrap();
        let second_cancel = second.cancel.clone();

        drop(first);
        assert!(!second_cancel.load(Ordering::SeqCst));
        {
            let cache = shared.architecture.lock().unwrap();
            assert_eq!(cache.generation, second.generation);
            assert!(
                cache
                    .cancel
                    .as_ref()
                    .is_some_and(|cancel| Arc::ptr_eq(cancel, &second_cancel))
            );
        }

        drop(second);
        assert!(shared.architecture.lock().unwrap().cancel.is_none());
    }

    #[test]
    fn workspace_changes_invalidate_the_ticket_and_drop_the_cached_model() {
        let shared = test_state();
        shared.architecture.lock().unwrap().cached =
            Some(cached_model("/workspace", "architecture"));
        let ticket = LoadTicket::new(shared.clone()).unwrap();
        let cancel = ticket.cancel.clone();

        cancel_for_workspace_change(&shared);

        assert!(cancel.load(Ordering::SeqCst));
        assert!(ticket.require_current().unwrap_err().contains("superseded"));
        {
            let cache = shared.architecture.lock().unwrap();
            assert!(cache.cached.is_none());
            assert!(cache.cancel.is_none());
        }

        let newer = LoadTicket::new(shared.clone()).unwrap();
        drop(ticket);
        let cache = shared.architecture.lock().unwrap();
        assert!(
            cache
                .cancel
                .as_ref()
                .is_some_and(|slot| Arc::ptr_eq(slot, &newer.cancel))
        );
    }

    #[test]
    fn shutdown_cancels_in_flight_work_but_keeps_the_cached_model() {
        let shared = test_state();
        shared.architecture.lock().unwrap().cached =
            Some(cached_model("/workspace", "architecture"));
        let ticket = LoadTicket::new(shared.clone()).unwrap();
        let cancel = ticket.cancel.clone();

        cancel_all(&shared);

        assert!(cancel.load(Ordering::SeqCst));
        assert!(ticket.require_current().is_err());
        let cache = shared.architecture.lock().unwrap();
        assert!(cache.cached.is_some());
        assert!(cache.cancel.is_none());
    }

    #[test]
    fn discovery_serializes_camel_case_and_null_fields() {
        let discovery = Discovery {
            workspace: "/workspace".into(),
            models: vec![ModelRoot {
                root: ".".into(),
                sources: vec!["model.c4".into()],
                config: None,
                unsupported_config: Some("likec4.config.ts".into()),
            }],
            default_path: DEFAULT_MODEL_PATH.into(),
            runtime: RuntimeStatus {
                ready: false,
                node: None,
                node_version: None,
                helper: None,
                message: "Node.js was not found.".into(),
            },
            warnings: vec!["warning".into()],
        };

        assert_eq!(
            serde_json::to_value(discovery).unwrap(),
            serde_json::json!({
                "workspace": "/workspace",
                "models": [{
                    "root": ".",
                    "sources": ["model.c4"],
                    "config": null,
                    "unsupportedConfig": "likec4.config.ts",
                }],
                "defaultPath": "architecture/model.c4",
                "runtime": {
                    "ready": false,
                    "node": null,
                    "nodeVersion": null,
                    "helper": null,
                    "message": "Node.js was not found.",
                },
                "warnings": ["warning"],
            })
        );
    }

    #[test]
    fn load_results_serialize_null_fields_and_cache_flags() {
        let empty = LoadResult {
            workspace: "/workspace".into(),
            root: ".".into(),
            sources: Vec::new(),
            views: Vec::new(),
            model: None,
            diagnostics: Vec::new(),
            stale: false,
            cached: false,
            source_digest: "digest".into(),
            error: Some("failed".into()),
            warnings: Vec::new(),
        };
        let value = serde_json::to_value(&empty).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 11);
        assert!(value["model"].is_null());
        assert_eq!(value["sourceDigest"], "digest");
        assert_eq!(value["stale"], false);
        assert_eq!(value["cached"], false);

        let cached = LoadResult {
            views: vec![ViewInfo {
                id: "index".into(),
                title: "Index".into(),
                kind: "element".into(),
            }],
            model: Some(serde_json::json!({ "_stage": "layouted" })),
            cached: true,
            error: None,
            ..empty
        };
        let value = serde_json::to_value(cached).unwrap();
        assert_eq!(value["cached"], true);
        assert_eq!(value["stale"], false);
        assert!(value["error"].is_null());
        assert_eq!(value["views"][0]["id"], "index");
        assert_eq!(value["model"]["_stage"], "layouted");

        let stale = fallback_result(
            LoadBase {
                workspace: "/workspace".into(),
                root: "architecture".into(),
                sources: Vec::new(),
                digest: "digest".into(),
                warnings: Vec::new(),
            },
            Some(cached_model("/workspace", "architecture")),
            vec![Diagnostic {
                message: "Target not resolved".into(),
                file: Some("architecture/model.c4".into()),
                line: Some(7),
                column: None,
            }],
            Some("The LikeC4 model has errors.".into()),
        );
        let value = serde_json::to_value(stale).unwrap();
        assert_eq!(value["cached"], false);
        assert_eq!(value["stale"], true);
        assert!(value["model"].is_object());
        assert_eq!(value["views"][0]["id"], "index");
        assert_eq!(value["diagnostics"][0]["file"], "architecture/model.c4");
        assert_eq!(value["diagnostics"][0]["column"], serde_json::Value::Null);
    }

    #[test]
    fn caps_diagnostics() {
        let diagnostics = (0..MAX_DIAGNOSTICS + 10)
            .map(|index| Diagnostic {
                message: "x".repeat(MAX_DIAGNOSTIC_CHARS + 10),
                file: Some(format!("model-{index}.c4")),
                line: Some(index as u32),
                column: None,
            })
            .collect();
        let diagnostics = cap_diagnostics(diagnostics);
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(diagnostics[0].message.contains("[truncated]"));
    }

    #[tokio::test]
    async fn compiles_cross_directory_references_without_a_root_file_or_config() {
        let node = find_node().expect("Node is required for the compiler integration test");
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../architecture-helper");
        let fixture = helper.join("test/fixtures/scattered");
        let workspace = Workspace::new(fixture.to_str().unwrap()).unwrap();
        let (models, _) = discover_models(&workspace, false).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].root, ".");
        assert_eq!(
            models[0].sources,
            vec!["docs/views.likec4", "services/api/model.c4", "web/model.c4"]
        );
        let (sources, _) = read_sources(&workspace, &models[0]).unwrap();
        let response = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": sources }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(response.views[0].id, "index");
        let model = response.model.unwrap();
        assert!(model["elements"].get("api").is_some());
        assert!(model["elements"].get("web").is_some());
        assert_eq!(model["relations"].as_object().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_pinned_helper_compiles_the_multi_file_fixture() {
        let Some(node) = find_node() else {
            return;
        };
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("architecture-helper");
        if !helper_ready(&helper) {
            return;
        }
        let fixture = helper.join("test/fixtures/valid");
        let workspace = Workspace::new(fixture.to_str().unwrap()).unwrap();
        let (models, _) = discover_models(&workspace, false).unwrap();
        let model = models
            .iter()
            .find(|model| model.root == "architecture")
            .unwrap();
        let (sources, infos) = read_sources(&workspace, model).unwrap();
        assert_eq!(infos.len(), 3);

        let response = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": sources }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(response.ok);
        assert_eq!(
            response
                .views
                .iter()
                .map(|view| view.id.as_str())
                .collect::<Vec<_>>(),
            vec!["index", "containers"]
        );
        assert_eq!(response.model.unwrap()["_stage"], "layouted");
    }
}
