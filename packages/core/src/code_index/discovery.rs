use std::collections::BTreeSet;
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
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub root: PathBuf,
    pub language: Language,
    /// A failed Cargo lookup is a routing barrier, not a guessed workspace root.
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
}

impl Discovery {
    pub(super) fn failed(workspace: PathBuf, error: String) -> Self {
        Self {
            workspace,
            projects: Vec::new(),
            incomplete: true,
            warnings: vec![error],
            files: BTreeSet::new(),
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
        let project = self
            .projects
            .iter()
            .filter(|project| project.language == language && file.starts_with(&project.root))
            .max_by_key(|project| project.root.components().count())
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
    };
    let mut cargo = Vec::new();
    let mut ts_roots = BTreeSet::new();
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
            _ => {}
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
        if !result.projects.iter().any(|project| project.root == root) {
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
