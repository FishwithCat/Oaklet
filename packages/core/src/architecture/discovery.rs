//! Workspace model discovery and source preparation.
//!
//! Discovery walks the selected workspace with the same ignore rules as the
//! repository. Source preparation validates containment and file limits before
//! the parent hands the text to the LikeC4 helper.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::{ModelRoot, SourceInfo};
use crate::tools::Workspace;

const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_MODEL_FILES: usize = 500;
const MAX_PROMPT_MODEL_SOURCES: usize = 5;
const MAX_SOURCE_FILES: usize = 200;
const MAX_SOURCE_FILE_BYTES: u64 = 1_000_000;
const MAX_SOURCE_BYTES: u64 = 4_000_000;

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

pub(super) fn discover_models(
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

pub(super) fn read_sources(
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

pub(super) fn digest_sources(sources: &BTreeMap<String, String>) -> String {
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

pub(super) fn normalize_root(root: &str) -> String {
    let mut root = root.trim().replace('\\', "/");
    while let Some(stripped) = root.strip_prefix("./") {
        root = stripped.to_string();
    }
    while root.len() > 1 && root.ends_with('/') {
        root.pop();
    }
    if root.is_empty() { ".".into() } else { root }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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

    #[test]
    fn normalizes_model_roots() {
        assert_eq!(normalize_root("./architecture/"), "architecture");
        assert_eq!(normalize_root(""), ".");
        assert_eq!(normalize_root("architecture\\views"), "architecture/views");
        assert_eq!(normalize_root("."), ".");
    }
}
