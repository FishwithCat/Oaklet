use super::{IndexError, Language, language_servers::LanguageServerManager};
use globset::{GlobBuilder, GlobMatcher};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

pub(super) struct FileWatcher {
    _watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
    overflow: Arc<AtomicBool>,
    root: PathBuf,
    servers: Arc<LanguageServerManager>,
}
#[derive(Default)]
pub(super) struct Batch {
    pub paths: BTreeSet<PathBuf>,
    pub reconcile: bool,
    pub error: Option<String>,
}
impl FileWatcher {
    pub async fn start(root: &Path, servers: Arc<LanguageServerManager>) -> Result<Self, String> {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || Self::new(&root, servers))
            .await
            .map_err(|error| error.to_string())?
    }
    pub fn new(root: &Path, servers: Arc<LanguageServerManager>) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let lost = overflow.clone();
        let watched_root = root.to_path_buf();
        let watched_servers = servers.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let event = match event {
                    Ok(mut event) => {
                        if matches!(event.kind, EventKind::Access(_)) {
                            return;
                        }
                        event
                            .paths
                            .retain(|path| relevant(&watched_root, path, &watched_servers));
                        if event.paths.is_empty() && !event.need_rescan() {
                            return;
                        }
                        Ok(event)
                    }
                    error => error,
                };
                if sender.try_send(event).is_err() {
                    lost.store(true, Ordering::Release);
                }
            })
            .map_err(|e| e.to_string())?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            _watcher: watcher,
            receiver,
            overflow,
            root: root.to_path_buf(),
            servers,
        })
    }
    pub async fn batch(&mut self) -> Batch {
        // A fixed window also observes overflow even if no later event can be enqueued.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut batch = Batch::default();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Ok(event) => {
                    if event.need_rescan() {
                        batch.reconcile = true;
                        batch.error = Some("File watcher requested a full reconciliation".into());
                    }
                    if matches!(event.kind, EventKind::Access(_)) {
                        continue;
                    }
                    for path in event.paths {
                        if relevant(&self.root, &path, &self.servers) {
                            batch.reconcile |= configuration(&path)
                                || matches!(
                                    event.kind,
                                    EventKind::Create(_)
                                        | EventKind::Remove(_)
                                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                                );
                            batch.paths.insert(path);
                        }
                    }
                }
                Err(error) => {
                    batch.error = Some(error.to_string());
                    batch.reconcile = true;
                }
            }
        }
        if self.overflow.swap(false, Ordering::AcqRel) {
            batch.reconcile = true;
            batch.error = Some("File watcher queue overflow; reconciling saved files".into());
        }
        batch
    }
}
pub(super) fn configuration(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    rust_configuration(path)
        || matches!(
            name,
            "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | ".gitignore"
                | ".ignore"
                | ".gitmodules"
                | "pyrightconfig.json"
                | "pyproject.toml"
                | "setup.cfg"
                | "setup.py"
                | "requirements.txt"
                | "poetry.lock"
                | "uv.lock"
                | "Pipfile"
                | "Pipfile.lock"
                | "go.work"
                | "go.work.sum"
                | "go.mod"
                | "go.sum"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts"
                | "gradle.properties"
                | "gradle.lockfile"
                | ".classpath"
                | ".project"
        )
        || (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json")
}
pub(super) fn configuration_language(path: &Path) -> Option<Language> {
    if rust_configuration(path) {
        return Some(Language::Rust);
    }
    match path.file_name()?.to_str()? {
        "pyrightconfig.json" | "pyproject.toml" | "setup.cfg" | "setup.py" | "requirements.txt"
        | "poetry.lock" | "uv.lock" | "Pipfile" | "Pipfile.lock" => Some(Language::Python),
        "go.work" | "go.work.sum" | "go.mod" | "go.sum" => Some(Language::Go),
        "pom.xml"
        | "build.gradle"
        | "build.gradle.kts"
        | "settings.gradle"
        | "settings.gradle.kts"
        | "gradle.properties"
        | "gradle.lockfile"
        | ".classpath"
        | ".project" => Some(Language::Java),
        ".gitignore" | ".ignore" | ".gitmodules" => None,
        _ => Some(Language::TypeScript),
    }
}
pub(super) fn rust_configuration(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
    ) || matches!(name, "config" | "config.toml")
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".cargo")
}
pub(super) fn temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(".#")
                || name.ends_with('~')
                || name.ends_with(".tmp")
                || name.ends_with(".swp")
                || name.ends_with(".swo")
        })
}
fn relevant(root: &Path, path: &Path, servers: &LanguageServerManager) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if temporary(path) {
        return false;
    }
    if relative.components().any(|c| {
        super::discovery::EXCLUDED
            .iter()
            .any(|name| c.as_os_str() == *name)
    }) {
        return false;
    }
    servers.watches(root, path)
        || Language::for_file(path).is_some()
        || configuration(path)
        || path.is_dir()
        || !path.exists() && path.extension().is_none()
}

struct Rule {
    base: PathBuf,
    pattern: GlobMatcher,
    kind: u64,
}
#[derive(Default)]
pub(super) struct Registrations(BTreeMap<String, Vec<Rule>>);
impl Registrations {
    pub fn register(&mut self, params: Value, root: &Path) -> Result<(), IndexError> {
        let mut additions = Vec::new();
        let registrations = params["registrations"]
            .as_array()
            .ok_or_else(|| IndexError::Invalid("Missing registrations".into()))?;
        for registration in registrations {
            if registration["method"] != "workspace/didChangeWatchedFiles" {
                return Err(IndexError::Unsupported(
                    "Unknown dynamic registration".into(),
                ));
            }
            let id = registration["id"]
                .as_str()
                .ok_or_else(|| IndexError::Invalid("Missing registration ID".into()))?;
            let watchers = registration["registerOptions"]["watchers"]
                .as_array()
                .ok_or_else(|| IndexError::Invalid("Missing watch patterns".into()))?;
            if watchers.len() > 256 || self.0.len() + additions.len() >= 64 {
                return Err(IndexError::Busy);
            }
            let mut rules = Vec::new();
            for watcher in watchers {
                let glob = &watcher["globPattern"];
                let (base, pattern) = if let Some(pattern) = glob.as_str() {
                    (root.to_path_buf(), pattern)
                } else {
                    let uri = glob["baseUri"]
                        .as_str()
                        .or_else(|| glob["baseUri"]["uri"].as_str())
                        .ok_or_else(|| IndexError::Invalid("Missing watch base URI".into()))?;
                    let base = lsp_types::Url::parse(uri)
                        .ok()
                        .and_then(|uri| uri.to_file_path().ok())
                        .ok_or_else(|| {
                            IndexError::Invalid("Watch base must be a file URI".into())
                        })?;
                    (
                        base,
                        glob["pattern"]
                            .as_str()
                            .ok_or_else(|| IndexError::Invalid("Missing watch pattern".into()))?,
                    )
                };
                if pattern.len() > 4096 {
                    return Err(IndexError::Invalid("Watch pattern too long".into()));
                }
                let pattern = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| IndexError::Invalid(e.to_string()))?
                    .compile_matcher();
                rules.push(Rule {
                    base,
                    pattern,
                    kind: watcher["kind"].as_u64().unwrap_or(7),
                });
            }
            additions.push((id.to_string(), rules));
        }
        self.0.extend(additions);
        Ok(())
    }
    pub fn unregister(&mut self, params: Value) {
        for registration in params["unregisterations"].as_array().into_iter().flatten() {
            if registration["method"] == "workspace/didChangeWatchedFiles"
                && let Some(id) = registration["id"].as_str()
            {
                self.0.remove(id);
            }
        }
    }
    pub fn matches(&self, path: &Path, kind: u64) -> bool {
        let mask = match kind {
            1 => 1,
            2 => 2,
            _ => 4,
        };
        self.0.values().flatten().any(|rule| {
            rule.kind & mask != 0
                && path.strip_prefix(&rule.base).is_ok_and(|relative| {
                    rule.pattern.is_match(relative) || rule.pattern.is_match(path)
                })
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn registrations_match_relative_globs_masks_and_unregister() {
        let mut rules = Registrations::default();
        rules.register(json!({"registrations":[{"id":"r","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":{"baseUri":"file:///workspace/","pattern":"**/*.{rs,toml}"},"kind":5}]}}]}), Path::new("/workspace")).unwrap();
        assert!(rules.matches(Path::new("/workspace/src/lib.rs"), 1));
        assert!(rules.matches(Path::new("/workspace/Cargo.toml"), 3));
        assert!(!rules.matches(Path::new("/workspace/src/lib.rs"), 2));
        assert!(!rules.matches(Path::new("/outside/src/lib.rs"), 1));
        rules.unregister(
            json!({"unregisterations":[{"id":"r","method":"workspace/didChangeWatchedFiles"}]}),
        );
        assert!(!rules.matches(Path::new("/workspace/src/lib.rs"), 1));
    }
    #[tokio::test]
    async fn overflow_and_errors_request_reconciliation_without_a_later_event() {
        let root = crate::tools::test_support::temp_root("index-watch-overflow");
        let mut watcher = FileWatcher::new(&root, LanguageServerManager::new()).unwrap();
        watcher.overflow.store(true, Ordering::Release);
        let batch = watcher.batch().await;
        assert!(batch.reconcile && batch.error.unwrap().contains("overflow"));
        let (sender, receiver) = mpsc::channel(1);
        watcher.receiver = receiver;
        sender
            .try_send(Err(notify::Error::generic("fixture watch failure")))
            .unwrap();
        let batch = watcher.batch().await;
        assert!(batch.reconcile && batch.error.unwrap().contains("fixture watch failure"));
        drop(watcher);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    #[test]
    fn toolchain_and_cargo_settings_participate_in_snapshot_invalidation() {
        for path in [
            "/project/rust-toolchain",
            "/project/rust-toolchain.toml",
            "/project/.cargo/config",
            "/project/.cargo/config.toml",
        ] {
            assert!(configuration(Path::new(path)) && rust_configuration(Path::new(path)));
        }
        assert!(!rust_configuration(Path::new("/project/config.toml")));
        assert!(!configuration(Path::new("/project/config.toml")));
    }
}
