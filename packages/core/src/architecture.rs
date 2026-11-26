//! Workspace architecture discovery and data-only LikeC4 preview loading.
//!
//! Discovery walks the selected workspace with the same ignore rules as the
//! repository. Loading reads the model sources, hands them to a pinned Node
//! helper over stdin, and returns the layouted JSON the React renderer needs.
//! The helper never reads the workspace or executes workspace configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, State, WebviewWindow};

use crate::SharedState;
use crate::tools::Workspace;

mod compiler;
mod discovery;
mod runtime;

use compiler::run_helper;
use discovery::{digest_sources, discover_models, normalize_root, read_sources};
use runtime::{runtime_status, runtime_status_for};

pub(crate) use compiler::{HelperResponse, read_capped, run_helper_watch};
pub use discovery::prompt_model_sources;
pub(crate) use runtime::{find_helper, find_node, run_node};

pub const DEFAULT_MODEL_PATH: &str = "architecture/model.c4";

const MAX_DIAGNOSTICS: usize = 50;
const MAX_DIAGNOSTIC_CHARS: usize = 2_000;
pub(crate) const MAX_SOURCE_FILE_BYTES: u64 = 1_000_000;

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

/// The model a planning run should validate against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanningScope {
    pub model_root: String,
    pub include_ignored: bool,
}

/// Current sources of the selected model plus the digest they produced.
#[derive(Debug)]
pub(crate) struct PlanningModel {
    pub(crate) root: String,
    pub(crate) sources: std::collections::BTreeMap<String, String>,
    pub(crate) digest: String,
    pub(crate) warnings: Vec<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
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
    cached: HashMap<(String, String), CachedModel>,
    loads: HashMap<String, LoadSlot>,
    /// Last model root loaded in the Architecture panel for each workspace.
    selected: HashMap<String, PlanningScope>,
    generation: u64,
}

struct LoadSlot {
    generation: u64,
    cancel: Arc<AtomicBool>,
}

/// One architecture load attempt. Creating it supersedes any older attempt and
/// dropping it releases the cancellation slot when this attempt still owns it.
struct LoadTicket {
    shared: Arc<crate::AppState>,
    workspace: String,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl LoadTicket {
    fn new(shared: Arc<crate::AppState>, workspace: String) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = {
            let mut cache = shared
                .architecture
                .lock()
                .map_err(|_| "Architecture state is unavailable.".to_string())?;
            if let Some(previous) = cache.loads.remove(&workspace) {
                previous.cancel.store(true, Ordering::SeqCst);
            }
            cache.generation = cache.generation.wrapping_add(1);
            let generation = cache.generation;
            cache.loads.insert(
                workspace.clone(),
                LoadSlot {
                    generation,
                    cancel: cancel.clone(),
                },
            );
            generation
        };
        Ok(Self {
            shared,
            workspace,
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
        if cache
            .loads
            .get(&self.workspace)
            .is_some_and(|slot| slot.generation == self.generation)
        {
            Ok(())
        } else {
            Err("Architecture load was superseded by a newer request.".into())
        }
    }
}

impl ArchitectureCache {
    /// The model currently selected in the Architecture panel, if any.
    pub(crate) fn selection(&self, workspace: &str) -> Option<PlanningScope> {
        self.selected.get(workspace).cloned()
    }
}

impl Drop for LoadTicket {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.shared.architecture.lock()
            && cache
                .loads
                .get(&self.workspace)
                .is_some_and(|slot| slot.generation == self.generation)
        {
            cache.loads.remove(&self.workspace);
        }
    }
}

/// Stop compiler work and drop cached models owned by one workspace.
pub fn cancel_workspace(state: &crate::AppState, workspace: &Path) {
    let workspace = workspace.to_string_lossy().into_owned();
    if let Ok(mut cache) = state.architecture.lock() {
        if let Some(slot) = cache.loads.remove(&workspace) {
            slot.cancel.store(true, Ordering::SeqCst);
        }
        cache.cached.retain(|(cached, _), _| cached != &workspace);
        cache.selected.remove(&workspace);
    }
}

/// Stop any compiler process when the application exits.
pub fn cancel_all(state: &crate::AppState) {
    if let Ok(mut cache) = state.architecture.lock() {
        for (_, slot) in cache.loads.drain() {
            slot.cancel.store(true, Ordering::SeqCst);
        }
        cache.selected.clear();
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
    let ticket = LoadTicket::new(shared.clone(), workspace_text.clone())?;

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
    if let Ok(mut cache) = shared.architecture.lock() {
        cache.selected.insert(
            workspace_text.clone(),
            PlanningScope {
                model_root: model.root.clone(),
                include_ignored,
            },
        );
    }

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
            Some("Node.js was not found. Install Node.js 22.23.3 or newer.".into()),
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

    ticket.require_current()?;

    let request = serde_json::json!({
        "version": 1,
        "sources": source_map,
    });
    let outcome = run_helper(&node, &helper, &request, ticket.cancel.clone()).await;

    ticket.require_current()?;

    let mut cache = shared
        .architecture
        .lock()
        .map_err(|_| "Architecture state is unavailable.".to_string())?;
    if cache
        .loads
        .get(&ticket.workspace)
        .is_none_or(|slot| slot.generation != ticket.generation)
    {
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

            cache.cached.insert(
                (base.workspace.clone(), base.root.clone()),
                CachedModel {
                    workspace: base.workspace.clone(),
                    root: base.root.clone(),
                    digest: base.digest.clone(),
                    model: model_data.clone(),
                    views: response.views.clone(),
                },
            );
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

/// Resolve one model for a planning run and read the exact sources a draft will
/// be validated against.
pub(crate) fn planning_model(
    workspace: &Workspace,
    requested: Option<&PlanningScope>,
) -> Result<PlanningModel, String> {
    let include_ignored = requested.is_some_and(|scope| scope.include_ignored);
    let (models, warnings) = discover_models(workspace, include_ignored)?;
    if models.is_empty() {
        return Err("No LikeC4 architecture model was found in this workspace.".into());
    }

    let requested_root = requested.map(|scope| normalize_root(&scope.model_root));
    let model = match requested_root.as_deref() {
        Some(root) => models
            .iter()
            .find(|model| model.root == root)
            .ok_or_else(|| {
                format!(
                    "The selected architecture model root was not found: {root}. Available models: {}.",
                    model_roots(&models)
                )
            })?,
        None if models.len() == 1 => &models[0],
        None => {
            return Err(format!(
                "This workspace has multiple LikeC4 models ({}). Ask the user to choose one in the Architecture panel, then retry.",
                model_roots(&models)
            ));
        }
    };

    let (sources, _) = read_sources(workspace, model)?;
    let digest = digest_sources(&sources);
    Ok(PlanningModel {
        root: model.root.clone(),
        sources,
        digest,
        warnings,
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCompare {
    pub model_root: String,
    pub source_digest: String,
    pub view_id: String,
    /// Layouted model with only the compare view, ready for the diagram.
    pub model: Value,
    /// Layouted compare view with `diffStatus` on added, removed, and modified items.
    pub view: Value,
}

/// Lay out one affected view for the plan review card.
///
/// The saved draft is recompiled against the current sources, and the call is
/// refused when the model no longer matches the plan's baseline. The desktop
/// binds the result to the same plan version before rendering it.
pub(crate) async fn plan_compare(
    app: &AppHandle,
    workspace_path: &Path,
    validation: &crate::store::PlanValidation,
    view_id: &str,
) -> Result<PlanCompare, String> {
    let changed_view = validation
        .diff
        .as_ref()
        .and_then(|diff| diff.get("views"))
        .and_then(Value::as_array)
        .is_some_and(|views| {
            views
                .iter()
                .any(|view| view.get("id").and_then(Value::as_str) == Some(view_id))
        });
    if !changed_view {
        return Err("This view is not part of the plan's architecture diff.".into());
    }
    if validation.dsl.trim().is_empty() {
        return Err("This plan has no architecture draft to preview.".into());
    }

    let workspace = Workspace::new(&workspace_path.to_string_lossy())?;
    let scope = PlanningScope {
        model_root: validation.model_root.clone(),
        include_ignored: validation.include_ignored,
    };
    let expected = validation.source_digest.clone();
    let model =
        tauri::async_runtime::spawn_blocking(move || planning_model(&workspace, Some(&scope)))
            .await
            .map_err(|error| format!("Could not read the architecture model: {error}"))??;
    if model.digest != expected {
        return Err(
            "The architecture model changed after this plan was validated. Ask for an updated plan, then preview the new version."
                .into(),
        );
    }

    let Some(node) = find_node() else {
        return Err("LikeC4 preview requires Node.js 22.23.3 or newer.".into());
    };
    let Some(helper) = find_helper(app) else {
        return Err("The LikeC4 compiler helper is missing. Reinstall Oaklet.".into());
    };
    let model_root = model.root.clone();
    let draft = crate::tools::draft_path(&model.root, &model.sources);
    let request = serde_json::json!({
        "version": 1,
        "sources": model.sources,
        "diff": { "path": draft, "text": validation.dsl },
        "compare": { "viewId": view_id },
    });
    let response = run_helper(&node, &helper, &request, Arc::new(AtomicBool::new(false))).await?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "LikeC4 could not build the compare view.".into()));
    }
    let Some(view) = response.view else {
        return Err("LikeC4 did not return a compare view.".into());
    };
    let Some(model) = response.model else {
        return Err("LikeC4 did not return the model for the compare view.".into());
    };
    Ok(PlanCompare {
        model_root,
        source_digest: expected,
        view_id: view_id.to_string(),
        model,
        view,
    })
}

fn model_roots(models: &[ModelRoot]) -> String {
    models
        .iter()
        .map(|model| format!("`{}`", model.root))
        .collect::<Vec<_>>()
        .join(", ")
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
        .get(&(workspace.to_string(), root.to_string()))
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

#[tauri::command]
pub(crate) async fn architecture_discover(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    include_ignored: bool,
    workspace: Option<String>,
) -> Result<Discovery, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || discover(&app, &workspace, include_ignored))
        .await
        .map_err(|error| format!("Could not scan the workspace: {error}"))?
}

#[tauri::command]
pub(crate) async fn architecture_load(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    root: String,
    include_ignored: bool,
    workspace: Option<String>,
) -> Result<LoadResult, String> {
    let workspace =
        crate::chat::worktree::view_directory(&state, window.label(), workspace.as_deref())?;
    let shared = state.inner().clone();
    load(app, shared, workspace, root, include_ignored).await
}

#[cfg(test)]
mod tests {
    use super::runtime::helper_ready;
    use super::*;
    use crate::tools::test_support::temp_root;

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
            cached: HashMap::from([(
                ("/workspace".into(), "architecture".into()),
                cached_model("/workspace", "architecture"),
            )]),
            ..Default::default()
        };

        assert!(cached_for(&cache, "/workspace", "architecture").is_some());
        assert!(cached_for(&cache, "/workspace", "docs").is_none());
        assert!(cached_for(&cache, "/other", "architecture").is_none());
    }

    #[test]
    fn planning_uses_the_selected_model_and_rejects_unknown_roots() {
        let root = temp_root("planning-scope");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::write(root.join("one/likec4.config.json"), r#"{"name":"one"}"#).unwrap();
        std::fs::write(root.join("one/model.c4"), "model { }").unwrap();
        std::fs::write(root.join("two/likec4.config.json"), r#"{"name":"two"}"#).unwrap();
        std::fs::write(root.join("two/model.c4"), "model { }").unwrap();
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();

        assert!(planning_model(&workspace, None).is_err());
        let scope = PlanningScope {
            model_root: "./two/".into(),
            include_ignored: false,
        };
        let model = planning_model(&workspace, Some(&scope)).unwrap();
        assert_eq!(model.root, "two");
        assert_eq!(model.sources.len(), 2);
        assert_eq!(model.digest.len(), 16);

        let missing = PlanningScope {
            model_root: "missing".into(),
            include_ignored: false,
        };
        let error = planning_model(&workspace, Some(&missing)).unwrap_err();
        assert!(error.contains("was not found"));
        assert!(error.contains("`one`"));

        std::fs::remove_dir_all(root).ok();
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
            exit: std::sync::Mutex::new(crate::shutdown::ExitState::Running),
            config: std::sync::Mutex::new(crate::AppConfig::default()),
            config_path: PathBuf::new(),
            config_write: std::sync::Mutex::new(()),
            windows: std::sync::Mutex::new(crate::WindowRegistry::default()),
            spill_root: PathBuf::new(),
            approvals: std::sync::Mutex::new(std::collections::HashMap::new()),
            chat: crate::chat::ChatState::in_memory(),
            command_allow_all: std::sync::Mutex::new(std::collections::HashSet::new()),
            terminal: std::sync::Mutex::new(std::collections::HashMap::new()),
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            architecture: std::sync::Mutex::new(ArchitectureCache::default()),
            ids: std::sync::atomic::AtomicU64::new(0),
        })
    }

    #[test]
    fn a_newer_ticket_supersedes_and_cancels_the_previous_one() {
        let shared = test_state();
        let first = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        let first_cancel = first.cancel.clone();

        let second = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();

        assert!(first_cancel.load(Ordering::SeqCst));
        assert!(first.require_current().unwrap_err().contains("superseded"));
        second.require_current().unwrap();
        assert_eq!(second.generation, first.generation + 1);
    }

    #[test]
    fn different_workspaces_load_without_cancelling_each_other() {
        let shared = test_state();
        let first = LoadTicket::new(shared.clone(), "/workspace-a".into()).unwrap();
        let first_cancel = first.cancel.clone();
        let second = LoadTicket::new(shared.clone(), "/workspace-b".into()).unwrap();

        assert!(!first_cancel.load(Ordering::SeqCst));
        first.require_current().unwrap();
        second.require_current().unwrap();
    }

    #[test]
    fn dropping_an_old_ticket_keeps_the_newer_cancellation_slot() {
        let shared = test_state();
        let first = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        let second = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        let second_cancel = second.cancel.clone();

        drop(first);
        assert!(!second_cancel.load(Ordering::SeqCst));
        {
            let cache = shared.architecture.lock().unwrap();
            assert_eq!(cache.generation, second.generation);
            assert!(
                cache
                    .loads
                    .get("/workspace")
                    .map(|slot| &slot.cancel)
                    .is_some_and(|cancel| Arc::ptr_eq(cancel, &second_cancel))
            );
        }

        drop(second);
        assert!(shared.architecture.lock().unwrap().loads.is_empty());
    }

    #[test]
    fn workspace_changes_invalidate_the_ticket_and_drop_the_cached_model() {
        let shared = test_state();
        shared.architecture.lock().unwrap().cached.insert(
            ("/workspace".into(), "architecture".into()),
            cached_model("/workspace", "architecture"),
        );
        let ticket = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        let cancel = ticket.cancel.clone();

        cancel_workspace(&shared, Path::new("/workspace"));

        assert!(cancel.load(Ordering::SeqCst));
        assert!(ticket.require_current().unwrap_err().contains("superseded"));
        {
            let cache = shared.architecture.lock().unwrap();
            assert!(cache.cached.is_empty());
            assert!(cache.loads.is_empty());
        }

        let newer = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        drop(ticket);
        let cache = shared.architecture.lock().unwrap();
        assert!(
            cache
                .loads
                .get("/workspace")
                .map(|slot| &slot.cancel)
                .is_some_and(|slot| Arc::ptr_eq(slot, &newer.cancel))
        );
    }

    #[test]
    fn shutdown_cancels_in_flight_work_but_keeps_the_cached_model() {
        let shared = test_state();
        shared.architecture.lock().unwrap().cached.insert(
            ("/workspace".into(), "architecture".into()),
            cached_model("/workspace", "architecture"),
        );
        let ticket = LoadTicket::new(shared.clone(), "/workspace".into()).unwrap();
        let cancel = ticket.cancel.clone();

        cancel_all(&shared);

        assert!(cancel.load(Ordering::SeqCst));
        assert!(ticket.require_current().is_err());
        let cache = shared.architecture.lock().unwrap();
        assert!(!cache.cached.is_empty());
        assert!(cache.loads.is_empty());
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
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../architecture-helper");
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
