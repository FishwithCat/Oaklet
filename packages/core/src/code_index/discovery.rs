use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use serde::Serialize;
use serde_json::Value;

use super::{Language, runtime};
use crate::tools::Workspace;

const MAX_ENTRIES: usize = 50_000;
const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const EXCLUDED: &[&str] = &[
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
    ".gradle",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub root: PathBuf,
    pub language: Language,
    /// A failed workspace lookup is a routing barrier, not a guessed project root.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub workspace: PathBuf,
    pub projects: Vec<Project>,
    pub incomplete: bool,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub(super) files: BTreeSet<PathBuf>,
    #[serde(skip)]
    pub(super) go_members: BTreeMap<PathBuf, PathBuf>,
}

impl Discovery {
    pub(super) fn failed(workspace: PathBuf, error: String) -> Self {
        Self {
            workspace,
            projects: Vec::new(),
            incomplete: true,
            warnings: vec![error],
            files: BTreeSet::new(),
            go_members: BTreeMap::new(),
        }
    }

    pub fn route(&self, input: &str) -> Result<&Project, String> {
        let workspace = Workspace::new(&self.workspace.to_string_lossy())?;
        let file = workspace.resolve(input)?;
        if !file.is_file() || !self.files.contains(&file) {
            return Err(
                "File is ignored, unsupported, or was not discovered; refresh project discovery."
                    .into(),
            );
        }
        let language = Language::for_file(&file).ok_or("Unsupported source language.")?;
        let owner = self
            .projects
            .iter()
            .filter(|project| project.language == language && file.starts_with(&project.root))
            .map(|project| (&project.root, &project.root))
            .chain(
                self.go_members
                    .iter()
                    .filter(|(member, _)| language == Language::Go && file.starts_with(member)),
            )
            .max_by_key(|(boundary, _)| boundary.components().count())
            .map(|(_, owner)| owner)
            .ok_or("No supported project owns this file.")?;
        let project = self
            .projects
            .iter()
            .find(|project| project.language == language && &project.root == owner)
            .ok_or("No supported project owns this file.")?;
        if let Some(error) = &project.error {
            return Err(error.clone());
        }
        Ok(project)
    }
}

pub async fn discover(workspace: &Path) -> Result<Discovery, String> {
    discover_with_cancel(workspace, None).await
}

pub(super) async fn discover_with_cancel(
    workspace: &Path,
    cancel: Option<crate::tools::Cancel>,
) -> Result<Discovery, String> {
    let workspace = Workspace::new(&workspace.to_string_lossy())?;
    let root = workspace.root().to_path_buf();
    let deadline = Instant::now() + SCAN_TIMEOUT;
    let scan_cancel = cancel.clone();
    let (mut result, manifests) =
        tokio::task::spawn_blocking(move || scan(root, MAX_ENTRIES, deadline, scan_cancel))
            .await
            .map_err(|error| format!("Project discovery failed: {error}"))?;
    let cargo = runtime::find_program(
        "cargo",
        workspace.root(),
        std::env::var_os("PATH").as_deref(),
    );
    let go = runtime::find_program("go", workspace.root(), std::env::var_os("PATH").as_deref());
    resolve_go_workspaces(&workspace, &mut result, go.as_deref(), deadline).await;
    for manifest in manifests {
        if cancel
            .as_ref()
            .is_some_and(crate::tools::Cancel::is_cancelled)
        {
            return Err("Project discovery cancelled".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let located = if remaining.is_zero() {
            result.incomplete = true;
            Err("Project discovery exceeded 30 seconds; Cargo root is unconfirmed.".into())
        } else {
            locate_cargo_root(
                &workspace,
                &manifest,
                cargo.as_deref(),
                remaining.min(Duration::from_secs(5)),
            )
            .await
        };
        let project = match located {
            Ok(root) => Project {
                root,
                language: Language::Rust,
                error: None,
            },
            Err(error) => {
                result.incomplete = true;
                result.warnings.push(format!(
                    "{}: {error}",
                    workspace.relative(&manifest).display()
                ));
                Project {
                    root: manifest.parent().unwrap().to_path_buf(),
                    language: Language::Rust,
                    error: Some(error),
                }
            }
        };
        if !result.projects.iter().any(|existing| {
            existing.root == project.root
                && existing.language == project.language
                && existing.error == project.error
        }) {
            result.projects.push(project);
        }
    }
    result
        .projects
        .sort_by(|left, right| (&left.root, left.language).cmp(&(&right.root, right.language)));
    Ok(result)
}

// Use Go's parser: quoted paths, comments and replace blocks follow the toolchain's grammar.
pub(super) async fn resolve_go_workspaces(
    workspace: &Workspace,
    result: &mut Discovery,
    go: Option<&Path>,
    deadline: Instant,
) {
    let manifests: Vec<_> = result
        .files
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "go.work"))
        .cloned()
        .collect();
    let modules: BTreeSet<_> = result
        .files
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "go.mod"))
        .map(|path| path.parent().unwrap().to_path_buf())
        .collect();
    for manifest in manifests {
        let root = manifest.parent().unwrap();
        let resolve = async {
            let go = go.ok_or("Go is missing; go.work membership is unconfirmed.")?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(
                    "Project discovery exceeded 30 seconds; go.work membership is unconfirmed."
                        .into(),
                );
            }
            let output = runtime::run(
                go,
                &[
                    "work".into(),
                    "edit".into(),
                    "-json".into(),
                    manifest.to_string_lossy().into_owned(),
                ],
                root,
                remaining.min(Duration::from_secs(5)),
            )
            .await?;
            let value: Value = serde_json::from_str(&output)
                .map_err(|error| format!("Invalid go.work response: {error}"))?;
            let mut members = Vec::new();
            for member in value["Use"].as_array().into_iter().flatten() {
                let path = member["DiskPath"]
                    .as_str()
                    .ok_or("Missing go.work member path")?;
                let member = workspace.resolve(&root.join(path).to_string_lossy())?;
                if !modules.contains(&member) {
                    return Err(format!(
                        "go.work member is missing or ignored: {}",
                        member.display()
                    ));
                }
                if result
                    .go_members
                    .get(&member)
                    .is_some_and(|owner| owner != root)
                {
                    return Err(format!(
                        "Module belongs to multiple go.work files: {}",
                        member.display()
                    ));
                }
                members.push(member);
            }
            Ok::<_, String>(members)
        }
        .await;
        match resolve {
            Ok(members) => {
                for member in members {
                    if member != root && !member.join("go.work").is_file() {
                        result.projects.retain(|project| {
                            project.language != Language::Go || project.root != member
                        });
                        result.go_members.insert(member, root.to_path_buf());
                    }
                }
            }
            Err(error) => {
                result.incomplete = true;
                result
                    .warnings
                    .push(format!("{}: {error}", manifest.display()));
                // Unknown membership must not silently start a server per descendant module.
                result.projects.retain(|project| {
                    project.language != Language::Go
                        || project.root == root
                        || !project.root.starts_with(root)
                        || project.root.join("go.work").is_file()
                });
                if let Some(project) = result
                    .projects
                    .iter_mut()
                    .find(|project| project.language == Language::Go && project.root == root)
                {
                    project.error = Some(error);
                }
            }
        }
    }
}

async fn locate_cargo_root(
    workspace: &Workspace,
    manifest: &Path,
    cargo: Option<&Path>,
    timeout: Duration,
) -> Result<PathBuf, String> {
    let cargo = cargo.ok_or("Cargo is missing; Rust workspace root is unconfirmed.")?;
    let args = vec![
        "locate-project".into(),
        "--workspace".into(),
        "--message-format=json".into(),
        "--manifest-path".into(),
        manifest.to_string_lossy().into_owned(),
    ];
    let output = runtime::run(cargo, &args, manifest.parent().unwrap(), timeout).await?;
    let value: Value = serde_json::from_str(&output)
        .map_err(|error| format!("Invalid Cargo root response: {error}"))?;
    let path = value["root"]
        .as_str()
        .ok_or("Cargo returned no root manifest.")?;
    let root_manifest = workspace.resolve(path)?;
    if !root_manifest.is_file() {
        return Err("Cargo root manifest is unavailable.".into());
    }
    Ok(root_manifest.parent().unwrap().to_path_buf())
}

fn scan(
    root: PathBuf,
    max_entries: usize,
    deadline: Instant,
    cancel: Option<crate::tools::Cancel>,
) -> (Discovery, Vec<PathBuf>) {
    let mut result = Discovery {
        workspace: root.clone(),
        projects: Vec::new(),
        incomplete: false,
        warnings: Vec::new(),
        files: BTreeSet::new(),
        go_members: BTreeMap::new(),
    };
    let mut cargo = Vec::new();
    let mut ts_roots = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .filter_entry(|entry| {
            entry.depth() == 0 || !EXCLUDED.iter().any(|name| entry.file_name() == *name)
        });
    for (index, entry) in walker.build().enumerate() {
        if cancel
            .as_ref()
            .is_some_and(crate::tools::Cancel::is_cancelled)
        {
            break;
        }
        if index >= max_entries || Instant::now() >= deadline {
            result.incomplete = true;
            result.warnings.push(
                "Project scan reached its entry or time limit; discovered projects are partial."
                    .into(),
            );
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result.incomplete = true;
                if result.warnings.len() < 20 {
                    result.warnings.push(error.to_string());
                }
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if super::watch::temporary(&path) {
            continue;
        }
        result.files.insert(path.clone());
        match path.file_name().and_then(|name| name.to_str()) {
            Some("Cargo.toml") => cargo.push(path),
            Some("package.json" | "tsconfig.json" | "jsconfig.json") => {
                ts_roots.insert(path.parent().unwrap().to_path_buf());
            }
            Some("pyrightconfig.json" | "pyproject.toml") => {
                roots.insert((path.parent().unwrap().to_path_buf(), Language::Python));
            }
            Some("go.work" | "go.mod") => {
                roots.insert((path.parent().unwrap().to_path_buf(), Language::Go));
            }
            Some(
                "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts",
            ) => {
                roots.insert((path.parent().unwrap().to_path_buf(), Language::Java));
            }
            _ => {}
        }
    }
    for language in [Language::Python, Language::Java] {
        if result.files.iter().any(|path| {
            Language::for_file(path) == Some(language)
                && !roots
                    .iter()
                    .any(|(root, candidate)| *candidate == language && path.starts_with(root))
        }) {
            roots.insert((root.clone(), language));
        }
    }
    // Gradle settings own their multi-project build; nested settings remain independent.
    let gradle_roots: Vec<_> = roots
        .iter()
        .filter(|(root, language)| {
            *language == Language::Java
                && (root.join("settings.gradle").is_file()
                    || root.join("settings.gradle.kts").is_file())
        })
        .map(|(root, _)| root.clone())
        .collect();
    roots.retain(|(root, language)| {
        *language != Language::Java
            || root.join("pom.xml").is_file()
            || gradle_roots.contains(root)
            || !gradle_roots
                .iter()
                .any(|settings| root.starts_with(settings))
    });
    result
        .projects
        .extend(roots.into_iter().map(|(root, language)| Project {
            root,
            language,
            error: None,
        }));
    for language in [Language::Python, Language::Java] {
        if result
            .projects
            .iter()
            .any(|project| project.language == language)
        {
            result.warnings.push(match language {
                Language::Python => "Python resolution uses Pyright project settings and its selected interpreter; missing dependencies, virtual environments and dynamic imports can limit results.",
                _ => "Java resolution depends on the installed JDK and local build dependencies; automatic build configuration updates and Gradle import are disabled to avoid executing project build scripts.",
            }.into());
        }
    }
    // Multiple tsconfig files within a package belong to the same server instance.
    let package_roots: BTreeSet<_> = ts_roots
        .iter()
        .filter(|root| root.join("package.json").is_file())
        .cloned()
        .collect();
    for root in ts_roots {
        let root = package_roots
            .iter()
            .filter(|package| root.starts_with(package))
            .max_by_key(|package| package.components().count())
            .cloned()
            .unwrap_or(root);
        if !result
            .projects
            .iter()
            .any(|project| project.root == root && project.language == Language::TypeScript)
        {
            result.projects.push(Project {
                root,
                language: Language::TypeScript,
                error: None,
            });
        }
    }
    cargo.sort();
    (result, cargo)
}

#[cfg(test)]
pub(super) fn scan_limited(root: PathBuf, entries: usize, deadline: Instant) -> Discovery {
    scan(root, entries, deadline, None).0
}
