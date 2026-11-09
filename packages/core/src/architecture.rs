//! Workspace architecture discovery and data-only LikeC4 preview loading.
//!
//! Discovery walks the selected workspace with the same ignore rules as the
//! repository. Loading reads the model sources, hands them to a pinned Node
//! helper over stdin, and returns the layouted JSON the React renderer needs.
//! The helper never reads the workspace or executes workspace configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::tools::Workspace;

pub const DEFAULT_MODEL_PATH: &str = "architecture/model.c4";

const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_MODEL_FILES: usize = 500;
const MAX_PROMPT_MODEL_SOURCES: usize = 5;
const MAX_SOURCE_FILES: usize = 200;
const MAX_SOURCE_FILE_BYTES: u64 = 1_000_000;
const MAX_SOURCE_BYTES: u64 = 4_000_000;
const MAX_DIAGNOSTICS: usize = 50;
const MAX_DIAGNOSTIC_CHARS: usize = 2_000;
const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;
const COMPILER_TIMEOUT: Duration = Duration::from_secs(60);

const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    ".pnpm-store",
];

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

    let runtime = runtime_status(&app);
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

    let Some(node) = find_node() else {
        return Ok(fallback_result(
            base,
            cached_match,
            Vec::new(),
            Some("Node.js was not found. Install Node.js 22.22.3 or newer.".into()),
        ));
    };
    let Some(helper) = find_helper(&app) else {
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
        .clone()
        .filter(|cached| cached.workspace == workspace && cached.root == root)
}

fn discover_models(
    workspace: &Workspace,
    include_ignored: bool,
) -> Result<(Vec<ModelRoot>, Vec<String>), String> {
    let files = walk_files(workspace.root(), include_ignored)?;
    let mut model_files = Vec::new();
    let mut configs: BTreeMap<String, ConfigFiles> = BTreeMap::new();

    for path in files {
        let Some(kind) = config_kind(&path) else {
            if is_model_file(&path) {
                model_files.push(path);
            }
            continue;
        };
        let relative = relative_string(&path, workspace.root());
        let directory = path_string(Path::new(&relative).parent().unwrap_or(Path::new("")));
        let entry = configs.entry(directory).or_default();
        match kind {
            ConfigKind::Json => entry.json = Some(relative),
            ConfigKind::Unsupported => entry.unsupported = Some(relative),
        }
    }

    if model_files.len() > MAX_MODEL_FILES {
        return Err(format!(
            "Found {} LikeC4 model files; the limit is {MAX_MODEL_FILES}. Choose a narrower workspace.",
            model_files.len()
        ));
    }

    let mut warnings = Vec::new();
    if include_ignored {
        warnings.push("Included files normally ignored by git and \".ignore\" rules.".into());
    }
    for config in configs.values() {
        if let Some(path) = &config.unsupported {
            warnings.push(format!(
                "{path} is executable LikeC4 configuration. Oaklet previews data-only JSON configuration and model sources, so this file was not run."
            ));
        }
    }

    let config_roots: BTreeSet<String> = configs.keys().cloned().collect();
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for path in &model_files {
        let relative = path.strip_prefix(workspace.root()).unwrap_or(path);
        let relative_text = path_string(relative);
        let root = deepest_config_root(relative, &config_roots).unwrap_or_else(|| ".".into());
        grouped.entry(root).or_default().push(relative_text);
    }

    for (root, config) in &configs {
        if !grouped.contains_key(root) {
            warnings.push(format!(
                "LikeC4 configuration at {} has no model files in its directory.",
                config
                    .json
                    .as_deref()
                    .or(config.unsupported.as_deref())
                    .unwrap_or(root)
            ));
        }
    }

    let mut models = Vec::with_capacity(grouped.len());
    for (root, mut sources) in grouped {
        sources.sort();
        sources.dedup();
        let config = configs.get(&root);
        models.push(ModelRoot {
            root,
            sources,
            config: config.and_then(|config| config.json.clone()),
            unsupported_config: config.and_then(|config| config.unsupported.clone()),
        });
    }

    Ok((models, warnings))
}

#[derive(Default)]
struct ConfigFiles {
    json: Option<String>,
    unsupported: Option<String>,
}

enum ConfigKind {
    Json,
    Unsupported,
}

fn walk_files(root: &Path, include_ignored: bool) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut entries = 0usize;
    for result in model_walker(root, include_ignored).build() {
        entries += 1;
        if entries > MAX_WALK_ENTRIES {
            return Err(format!(
                "Stopped after {MAX_WALK_ENTRIES} entries. Choose a narrower workspace to scan for LikeC4 models."
            ));
        }
        let Ok(entry) = result else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if config_kind(entry.path()).is_some() || is_model_file(entry.path()) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn model_walker(root: &Path, include_ignored: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .parents(!include_ignored)
        .ignore(!include_ignored)
        .git_ignore(!include_ignored)
        .git_global(!include_ignored)
        .git_exclude(!include_ignored)
        .filter_entry(keep_entry);
    builder
}

/// A short list of LikeC4 sources used to tell the agent that this workspace
/// has an architecture model to keep in sync after code edits.
///
/// The walk uses the same ignore rules and directory exclusions as the
/// Architecture view, stops at the first few sources, and returns nothing when
/// the workspace cannot be opened.
pub fn prompt_model_sources(workspace_path: &Path) -> Vec<String> {
    let Ok(workspace) = Workspace::new(&workspace_path.to_string_lossy()) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    let mut entries = 0usize;
    for result in model_walker(workspace.root(), false).build() {
        entries += 1;
        if entries > MAX_WALK_ENTRIES {
            break;
        }
        let Ok(entry) = result else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if is_model_file(entry.path()) {
            sources.push(relative_string(entry.path(), workspace.root()));
            if sources.len() == MAX_PROMPT_MODEL_SOURCES {
                break;
            }
        }
    }
    sources.sort();
    sources
}

fn keep_entry(entry: &ignore::DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !EXCLUDED_DIRECTORIES
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn is_model_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("c4") || extension.eq_ignore_ascii_case("likec4")
        })
}

fn config_kind(path: &Path) -> Option<ConfigKind> {
    let name = path.file_name()?.to_str()?;
    if name == "likec4.config.json" {
        Some(ConfigKind::Json)
    } else if name.starts_with("likec4.config.") {
        Some(ConfigKind::Unsupported)
    } else {
        None
    }
}

fn relative_string(path: &Path, root: &Path) -> String {
    path_string(path.strip_prefix(root).unwrap_or(path))
}

fn path_string(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        ".".into()
    } else {
        path.to_string_lossy().replace('\\', "/")
    }
}

fn deepest_config_root(relative: &Path, config_roots: &BTreeSet<String>) -> Option<String> {
    let mut current = relative.parent().unwrap_or(Path::new("")).to_path_buf();
    loop {
        let key = path_string(&current);
        if config_roots.contains(&key) {
            return Some(key);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn read_sources(
    workspace: &Workspace,
    model: &ModelRoot,
) -> Result<(BTreeMap<String, String>, Vec<SourceInfo>), String> {
    let root_path = if model.root == "." {
        workspace.root().to_path_buf()
    } else {
        workspace.resolve(&model.root)?
    };
    if !root_path.is_dir() {
        return Err(format!("Not a model directory: {}", model.root));
    }

    let mut paths = model.sources.clone();
    if let Some(config) = &model.config {
        paths.push(config.clone());
    }
    paths.sort();
    paths.dedup();

    if paths.len() > MAX_SOURCE_FILES {
        return Err(format!(
            "This model has {} source files; the limit is {MAX_SOURCE_FILES}.",
            paths.len()
        ));
    }

    let mut sources = BTreeMap::new();
    let mut infos = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    for path in paths {
        let resolved = workspace.resolve(&path)?;
        if !resolved.starts_with(&root_path) {
            return Err(format!("Path is outside the selected model: {path}"));
        }
        let meta =
            fs::metadata(&resolved).map_err(|error| format!("Cannot read {path}: {error}"))?;
        if !meta.is_file() {
            return Err(format!("Not a file: {path}"));
        }
        if meta.len() > MAX_SOURCE_FILE_BYTES {
            return Err(format!(
                "{path} is {} bytes; a model source must be at most {MAX_SOURCE_FILE_BYTES} bytes.",
                meta.len()
            ));
        }
        total = total
            .checked_add(meta.len())
            .ok_or_else(|| "Model size overflow.".to_string())?;
        if total > MAX_SOURCE_BYTES {
            return Err(format!(
                "The model exceeds the {MAX_SOURCE_BYTES}-byte source limit."
            ));
        }
        let text = fs::read_to_string(&resolved)
            .map_err(|error| format!("Cannot read {path} as UTF-8 text: {error}"))?;
        infos.push(SourceInfo {
            path: path.clone(),
            bytes: text.len() as u64,
        });
        sources.insert(path, text);
    }
    Ok((sources, infos))
}

fn digest_sources(sources: &BTreeMap<String, String>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, content) in sources {
        for byte in path
            .bytes()
            .chain(std::iter::once(0))
            .chain(content.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn normalize_root(root: &str) -> String {
    let mut root = root.trim().replace('\\', "/");
    while let Some(stripped) = root.strip_prefix("./") {
        root = stripped.to_string();
    }
    while root.len() > 1 && root.ends_with('/') {
        root.pop();
    }
    if root.is_empty() { ".".into() } else { root }
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

pub(crate) fn find_node() -> Option<PathBuf> {
    if let Some(path) = env::var_os("OAKLET_NODE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = BTreeSet::new();

    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            candidates.insert(directory.join(node_name()));
        }
    }

    #[cfg(windows)]
    {
        if let Some(program_files) = env::var_os("ProgramFiles") {
            candidates.insert(PathBuf::from(program_files).join("nodejs").join("node.exe"));
        }
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            candidates.insert(
                PathBuf::from(local_app_data)
                    .join("Programs")
                    .join("nodejs")
                    .join("node.exe"),
            );
        }
        if let Some(nvm_symlink) = env::var_os("NVM_SYMLINK") {
            candidates.insert(PathBuf::from(nvm_symlink).join("node.exe"));
        }
    }

    #[cfg(not(windows))]
    {
        for path in [
            "/usr/local/bin/node",
            "/opt/homebrew/bin/node",
            "/opt/local/bin/node",
            "/usr/bin/node",
        ] {
            candidates.insert(PathBuf::from(path));
        }
    }

    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        if cfg!(windows) {
            candidates.insert(home.join("AppData/Roaming/npm/node.exe"));
        } else {
            candidates.insert(home.join(".volta/bin/node"));
            candidates.insert(home.join(".local/bin/node"));
            add_versioned_nodes(
                &mut candidates,
                &home.join(".nvm/versions/node"),
                "bin/node",
            );
            add_versioned_nodes(
                &mut candidates,
                &home.join(".local/share/fnm/node-versions"),
                "installation/bin/node",
            );
        }
    }

    let mut fallback = None;
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if let Some(version) = node_version(&candidate)
            && node_supported(&version)
        {
            return Some(candidate);
        }
        fallback.get_or_insert(candidate);
    }
    fallback
}

fn node_name() -> &'static str {
    if cfg!(windows) { "node.exe" } else { "node" }
}

fn add_versioned_nodes(candidates: &mut BTreeSet<PathBuf>, root: &Path, suffix: &str) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        candidates.insert(entry.path().join(suffix));
    }
}

fn node_version(node: &Path) -> Option<String> {
    let output = std::process::Command::new(node)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn node_supported(version: &str) -> bool {
    let version = version.trim_start_matches('v');
    let mut parts = version.split('.');
    let major: u64 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let patch: u64 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    (major, minor, patch) >= (22, 22, 3)
}

fn helper_ready(helper: &Path) -> bool {
    helper.join("src").join("compile.mjs").is_file()
        && helper
            .join("node_modules")
            .join("@likec4")
            .join("language-services")
            .join("package.json")
            .is_file()
}

pub(crate) fn find_helper(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = env::var_os("OAKLET_ARCHITECTURE_HELPER") {
        let path = PathBuf::from(path);
        if helper_ready(&path) {
            return Some(path);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(resource_directory) = app.path().resource_dir() {
        candidates.push(resource_directory.join("architecture-helper"));
    }
    #[cfg(debug_assertions)]
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("architecture-helper"),
    );
    if let Ok(executable) = env::current_exe() {
        for ancestor in executable.ancestors().skip(1).take(5) {
            candidates.push(ancestor.join("architecture-helper"));
            candidates.push(ancestor.join("Resources").join("architecture-helper"));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| helper_ready(candidate))
        .map(|candidate| candidate.canonicalize().unwrap_or(candidate))
}

fn runtime_status(app: &AppHandle) -> RuntimeStatus {
    runtime_status_for(find_node(), find_helper(app))
}

fn runtime_status_for(node: Option<PathBuf>, helper: Option<PathBuf>) -> RuntimeStatus {
    let helper = helper.map(|path| path.to_string_lossy().into_owned());
    let Some(node) = node else {
        return RuntimeStatus {
            ready: false,
            node: None,
            node_version: None,
            helper,
            message:
                "Node.js was not found. Install Node.js 22.22.3 or newer, or set OAKLET_NODE to the executable."
                    .into(),
        };
    };
    let Some(version) = node_version(&node) else {
        return RuntimeStatus {
            ready: false,
            node: Some(node.to_string_lossy().into_owned()),
            node_version: None,
            helper,
            message: "Node.js was found but did not run. Check the Node.js installation.".into(),
        };
    };
    if !node_supported(&version) {
        return RuntimeStatus {
            ready: false,
            node: Some(node.to_string_lossy().into_owned()),
            node_version: Some(version.clone()),
            helper,
            message: format!("Node.js {version} is too old. Install Node.js 22.22.3 or newer."),
        };
    }
    let Some(helper) = helper else {
        return RuntimeStatus {
            ready: false,
            node: Some(node.to_string_lossy().into_owned()),
            node_version: Some(version),
            helper: None,
            message:
                "The LikeC4 compiler helper is missing. Reinstall Oaklet or set OAKLET_ARCHITECTURE_HELPER."
                    .into(),
        };
    };
    RuntimeStatus {
        ready: true,
        node: Some(node.to_string_lossy().into_owned()),
        node_version: Some(version.clone()),
        helper: Some(helper),
        message: format!("Node.js {version} and the pinned LikeC4 compiler are ready."),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperResponse {
    ok: bool,
    #[serde(default)]
    model: Option<Value>,
    #[serde(default)]
    views: Vec<ViewInfo>,
    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    error: Option<String>,
}

async fn run_helper(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<HelperResponse, String> {
    run_helper_with_timeout(node, helper, request, cancel, COMPILER_TIMEOUT).await
}

async fn run_helper_with_timeout(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<HelperResponse, String> {
    let script = helper.join("src").join("compile.mjs");
    let mut child = Command::new(node)
        .arg(&script)
        .current_dir(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Could not start the LikeC4 compiler: {error}"))?;

    let request = serde_json::to_vec(request)
        .map_err(|error| format!("Could not encode the architecture request: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Could not open the LikeC4 compiler input.".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not read the LikeC4 compiler output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not read the LikeC4 compiler error output.".to_string())?;

    let work = async move {
        stdin
            .write_all(&request)
            .await
            .map_err(|error| format!("Could not send the model to the LikeC4 compiler: {error}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| format!("Could not finish the LikeC4 compiler input: {error}"))?;
        drop(stdin);

        let read_stdout = read_capped(stdout, MAX_HELPER_OUTPUT_BYTES);
        let read_stderr = read_capped(stderr, MAX_HELPER_STDERR_BYTES);
        let wait = async {
            child
                .wait()
                .await
                .map_err(|error| format!("Could not wait for the LikeC4 compiler: {error}"))
        };
        let (out, err, status) = tokio::try_join!(read_stdout, read_stderr, wait)?;
        Ok::<_, String>((out, err, status))
    };

    let outcome = tokio::select! {
        _ = wait_cancelled(cancel) => {
            return Err("Architecture loading was cancelled.".into());
        }
        result = tokio::time::timeout(timeout, work) => {
            match result {
                Ok(result) => result?,
                Err(_) => {
                    return Err(format!(
                        "The LikeC4 compiler did not finish within {} seconds.",
                        timeout.as_secs()
                    ));
                }
            }
        }
    };
    let (stdout, stderr, status) = outcome;

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            format!("exit status {status}")
        } else {
            stderr.to_string()
        };
        return Err(format!("The LikeC4 compiler failed: {detail}"));
    }

    let stdout = String::from_utf8(stdout)
        .map_err(|_| "The LikeC4 compiler returned non-UTF-8 output.".to_string())?;
    let response: HelperResponse = serde_json::from_str(&stdout)
        .map_err(|error| format!("Could not read the LikeC4 compiler result: {error}"))?;
    Ok(response)
}

pub(crate) async fn read_capped<R>(reader: R, limit: usize) -> Result<Vec<u8>, String>
where
    R: AsyncReadExt + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|error| format!("Could not read the LikeC4 helper output: {error}"))?;
    if output.len() > limit {
        return Err(format!(
            "The LikeC4 helper output exceeded the {limit}-byte limit."
        ));
    }
    Ok(output)
}

async fn wait_cancelled(cancel: Arc<AtomicBool>) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_root;

    fn workspace(tag: &str) -> (Workspace, PathBuf) {
        let root = temp_root(tag);
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        (workspace, root)
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn model(root: &str, sources: &[&str]) -> ModelRoot {
        ModelRoot {
            root: root.into(),
            sources: sources.iter().map(|source| source.to_string()).collect(),
            config: None,
            unsupported_config: None,
        }
    }

    #[test]
    fn discovers_models_and_honors_ignore_rules_without_git() {
        let (workspace, root) = workspace("arch-discover");
        write(&root, ".gitignore", "ignored/\n");
        write(&root, "architecture/model.c4", "model { }");
        write(&root, "architecture/views.c4", "views { }");
        write(&root, "ignored/model.c4", "model { }");
        write(&root, "node_modules/pkg/model.c4", "model { }");
        write(&root, ".git/hooks/model.c4", "model { }");
        write(&root, "docs/model.c4", "model { }");

        let (models, _) = discover_models(&workspace, false).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.root.as_str())
                .collect::<Vec<_>>(),
            vec!["."]
        );
        assert_eq!(
            models[0].sources,
            vec![
                "architecture/model.c4",
                "architecture/views.c4",
                "docs/model.c4"
            ]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prompt_model_sources_honor_ignore_rules() {
        let (workspace, root) = workspace("arch-prompt-sources");
        write(&root, ".gitignore", "ignored/\n");
        write(&root, "architecture/model.c4", "model { }");
        write(&root, "docs/views.likec4", "views { }");
        write(&root, "ignored/model.c4", "model { }");
        write(&root, "node_modules/pkg/model.c4", "model { }");
        write(&root, "README.md", "not a model");

        assert_eq!(
            prompt_model_sources(workspace.root()),
            vec!["architecture/model.c4", "docs/views.likec4"]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prompt_model_sources_stop_at_the_prompt_limit() {
        let (workspace, root) = workspace("arch-prompt-limit");
        for index in 0..MAX_PROMPT_MODEL_SOURCES + 2 {
            write(&root, &format!("models/{index}.c4"), "model { }");
        }

        assert_eq!(
            prompt_model_sources(workspace.root()).len(),
            MAX_PROMPT_MODEL_SOURCES
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn includes_ignored_models_when_explicitly_requested() {
        let (workspace, root) = workspace("arch-ignored");
        write(&root, ".gitignore", "ignored/\n");
        write(&root, "ignored/model.c4", "model { }");

        let (models, warnings) = discover_models(&workspace, true).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].root, ".");
        assert!(warnings.iter().any(|warning| warning.contains("ignored")));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn groups_nested_sources_under_a_json_config_root() {
        let (workspace, root) = workspace("arch-config");
        write(
            &root,
            "architecture/likec4.config.json",
            r#"{ "name": "fixture" }"#,
        );
        write(&root, "architecture/model.c4", "model { }");
        write(&root, "architecture/sub/views.c4", "views { }");
        write(&root, "docs/model.c4", "model { }");

        let (models, warnings) = discover_models(&workspace, false).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].root, ".");
        assert_eq!(models[0].sources, vec!["docs/model.c4"]);
        let architecture = models
            .iter()
            .find(|model| model.root == "architecture")
            .unwrap();
        assert_eq!(
            architecture.config.as_deref(),
            Some("architecture/likec4.config.json")
        );
        assert_eq!(
            architecture.sources,
            vec!["architecture/model.c4", "architecture/sub/views.c4"]
        );
        assert!(warnings.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nested_configs_separate_projects_from_the_default_model() {
        let (workspace, root) = workspace("arch-nested-configs");
        write(&root, "loose/model.c4", "model { }");
        write(&root, "docs/views.likec4", "views { }");
        write(&root, "parent/likec4.config.json", r#"{"name":"parent"}"#);
        write(&root, "parent/model.c4", "model { }");
        write(
            &root,
            "parent/child/likec4.config.json",
            r#"{"name":"child"}"#,
        );
        write(&root, "parent/child/model.c4", "model { }");

        for root_config in [false, true] {
            if root_config {
                write(&root, "likec4.config.json", r#"{"name":"root"}"#);
            }
            let (models, warnings) = discover_models(&workspace, false).unwrap();
            assert!(warnings.is_empty());
            assert_eq!(models.len(), 3);
            assert_eq!(models[0].root, ".");
            assert_eq!(
                models[0].sources,
                vec!["docs/views.likec4", "loose/model.c4"]
            );
            assert_eq!(
                models[0].config.as_deref(),
                root_config.then_some("likec4.config.json")
            );
            assert_eq!(models[1].root, "parent");
            assert_eq!(models[1].sources, vec!["parent/model.c4"]);
            assert_eq!(models[2].root, "parent/child");
            assert_eq!(models[2].sources, vec!["parent/child/model.c4"]);
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_executable_configuration_without_running_it() {
        let (workspace, root) = workspace("arch-ts-config");
        write(&root, "architecture/likec4.config.ts", "export default {}");
        write(&root, "architecture/model.c4", "model { }");

        let (models, warnings) = discover_models(&workspace, false).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].unsupported_config.as_deref(),
            Some("architecture/likec4.config.ts")
        );
        assert!(warnings.iter().any(|warning| warning.contains("not run")));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_root_level_model_groups_other_model_directories() {
        let (workspace, root) = workspace("arch-root-model");
        write(&root, "model.c4", "model { }");
        write(&root, "views/views.c4", "views { }");
        write(&root, "other/model.c4", "model { }");

        let (models, _) = discover_models(&workspace, false).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].root, ".");
        assert_eq!(
            models[0].sources,
            vec!["model.c4", "other/model.c4", "views/views.c4"]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discovers_no_models_in_a_workspace_without_one() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("architecture-helper/test/fixtures/no-model");
        let workspace = Workspace::new(fixture.to_str().unwrap()).unwrap();

        let (models, warnings) = discover_models(&workspace, false).unwrap();
        assert!(models.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_model_roots_that_leave_the_workspace() {
        let (workspace, root) = workspace("arch-escape");
        let error = read_sources(&workspace, &model("../", &[])).unwrap_err();
        assert!(error.contains("outside") || error.contains("Cannot resolve"));

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_sources_that_leave_the_workspace() {
        let (workspace, root) = workspace("arch-symlink");
        let outside = temp_root("arch-symlink-target");
        write(&outside, "secret.c4", "model { }");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let error = read_sources(&workspace, &model(".", &["link/secret.c4"])).unwrap_err();
        assert!(error.contains("outside") || error.contains("Cannot resolve"));

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn bounds_source_count_and_size() {
        let (workspace, root) = workspace("arch-limits");
        fs::create_dir_all(root.join("architecture")).unwrap();
        let too_many: Vec<String> = (0..=MAX_SOURCE_FILES)
            .map(|index| format!("architecture/model-{index}.c4"))
            .collect();
        let too_many_refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
        let error = read_sources(&workspace, &model("architecture", &too_many_refs)).unwrap_err();
        assert!(error.contains("limit"));

        write(
            &root,
            "architecture/large.c4",
            &"x".repeat(MAX_SOURCE_FILE_BYTES as usize + 1),
        );
        let error = read_sources(
            &workspace,
            &model("architecture", &["architecture/large.c4"]),
        )
        .unwrap_err();
        assert!(error.contains("at most"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn digest_tracks_source_content_and_paths() {
        let mut first = BTreeMap::new();
        first.insert("architecture/model.c4".to_string(), "model { }".to_string());
        let mut second = first.clone();
        assert_eq!(digest_sources(&first), digest_sources(&second));

        second.insert(
            "architecture/model.c4".to_string(),
            "model { api = system 'API' }".to_string(),
        );
        assert_ne!(digest_sources(&first), digest_sources(&second));

        let mut third = BTreeMap::new();
        third.insert("architecture/other.c4".to_string(), "model { }".to_string());
        assert_ne!(digest_sources(&first), digest_sources(&third));
    }

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
            runs: std::sync::Mutex::new(std::collections::HashMap::new()),
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
    fn normalizes_model_roots() {
        assert_eq!(normalize_root("./architecture/"), "architecture");
        assert_eq!(normalize_root(""), ".");
        assert_eq!(normalize_root("architecture\\views"), "architecture/views");
        assert_eq!(normalize_root("."), ".");
    }

    #[test]
    fn runtime_status_explains_missing_runtime() {
        let status = runtime_status_for(None, None);
        assert!(!status.ready);
        assert!(status.message.contains("Node.js"));

        let status = runtime_status_for(Some(PathBuf::from("/missing/node")), None);
        assert!(!status.ready);
        assert!(status.node_version.is_none());
    }

    #[test]
    fn node_version_gate_matches_the_helper_requirement() {
        assert!(node_supported("v22.22.3"));
        assert!(node_supported("v22.23.0"));
        assert!(node_supported("v26.13.0"));
        assert!(!node_supported("v22.22.2"));
        assert!(!node_supported("v22.12.0"));
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

    fn node_binary() -> Option<PathBuf> {
        find_node()
    }

    fn fake_helper(tag: &str, script: &str) -> PathBuf {
        let root = temp_root(tag);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/compile.mjs"), script).unwrap();
        root
    }

    #[tokio::test]
    async fn helper_success_is_parsed() {
        let Some(node) = node_binary() else {
            return;
        };
        let helper = fake_helper(
            "arch-helper-ok",
            r#"process.stdin.resume(); process.stdin.on("end", () => process.stdout.write(JSON.stringify({ok:true,model:{_stage:"layouted"},views:[{id:"index",title:"Index",kind:"element"}],diagnostics:[]})));"#,
        );
        let response = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(response.ok);
        assert_eq!(response.model.unwrap()["_stage"], "layouted");
        assert_eq!(response.views[0].id, "index");
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_failure_reports_stderr() {
        let Some(node) = node_binary() else {
            return;
        };
        let helper = fake_helper(
            "arch-helper-fail",
            r#"process.stderr.write("compiler exploded"); process.exit(2);"#,
        );
        let error = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("compiler exploded"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_times_out_and_is_killed() {
        let Some(node) = node_binary() else {
            return;
        };
        let helper = fake_helper("arch-helper-timeout", r#"setInterval(() => {}, 1000);"#);
        let error = run_helper_with_timeout(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
        assert!(error.contains("did not finish"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_cancellation_stops_the_process() {
        let Some(node) = node_binary() else {
            return;
        };
        let helper = fake_helper("arch-helper-cancel", r#"setInterval(() => {}, 1000);"#);
        let cancel = Arc::new(AtomicBool::new(true));
        let error = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            cancel,
        )
        .await
        .unwrap_err();
        assert!(error.contains("cancelled"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn compiler_output_is_bounded() {
        let output = vec![b'x'; 32];
        let error = read_capped(output.as_slice(), 16).await.unwrap_err();
        assert!(error.contains("exceeded"));
    }

    #[tokio::test]
    async fn compiles_cross_directory_references_without_a_root_file_or_config() {
        let node = node_binary().expect("Node is required for the compiler integration test");
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
        let Some(node) = node_binary() else {
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
