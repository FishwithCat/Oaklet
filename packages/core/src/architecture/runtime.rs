//! Node.js and LikeC4 helper runtime discovery.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use super::RuntimeStatus;

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

pub(super) fn helper_ready(helper: &Path) -> bool {
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

pub(super) fn runtime_status(app: &AppHandle) -> RuntimeStatus {
    let node = find_node();
    let helper = find_helper(app);
    runtime_status_for(node.as_deref(), helper.as_deref())
}

pub(super) fn runtime_status_for(node: Option<&Path>, helper: Option<&Path>) -> RuntimeStatus {
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
    let Some(version) = node_version(node) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_explains_missing_runtime() {
        let status = runtime_status_for(None, None);
        assert!(!status.ready);
        assert!(status.message.contains("Node.js"));

        let status = runtime_status_for(Some(Path::new("/missing/node")), None);
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
}
