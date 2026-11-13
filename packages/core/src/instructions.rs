//! Workspace instruction files: `AGENTS.md` and `CLAUDE.md` discovery.
//!
//! The baseline chain runs from the project root down to the workspace at run
//! start. A successful read/write/edit into a deeper directory loads that
//! directory's instructions before the next request. There is no watcher and no
//! resume reconciliation: every run starts from the files currently on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Larger files are skipped instead of consuming the system prompt budget.
const MAX_INSTRUCTION_BYTES: u64 = 256 * 1024;
const CANDIDATES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

struct InstructionFile {
    path: PathBuf,
    display: String,
    content: String,
}

/// Instruction files loaded for one run and the directories already scanned.
pub(crate) struct WorkspaceInstructions {
    project_root: PathBuf,
    files: Vec<InstructionFile>,
    scanned: HashSet<PathBuf>,
}

impl WorkspaceInstructions {
    /// Load the project-root-to-workspace chain.
    pub(crate) fn load(workspace: &Path) -> Self {
        let project_root = find_project_root(workspace);
        let mut instructions = Self {
            project_root: project_root.clone(),
            files: Vec::new(),
            scanned: HashSet::new(),
        };
        for directory in root_to_workspace(&project_root, workspace) {
            instructions.scan(&directory, &project_root);
        }
        instructions
    }

    /// Load instructions from the directories between the workspace and a
    /// touched file. Returns true when the system prompt gained new content.
    pub(crate) fn touch(&mut self, workspace: &Path, touched: &Path) -> bool {
        if !touched.starts_with(workspace) {
            return false;
        }
        let Some(parent) = touched.parent() else {
            return false;
        };
        let mut directories = Vec::new();
        let mut directory = parent;
        while directory.starts_with(workspace) && directory != workspace {
            directories.push(directory.to_path_buf());
            let Some(next) = directory.parent() else {
                break;
            };
            directory = next;
        }
        directories.reverse();

        let before = self.files.len();
        let project_root = self.project_root.clone();
        for directory in directories {
            if !self.scanned.contains(&directory) {
                self.scan(&directory, &project_root);
            }
        }
        self.files.len() > before
    }

    /// Model-facing instruction block, or `None` when nothing was found.
    pub(crate) fn render(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let mut text = String::from(
            "The following workspace instructions apply to this project. More specific files \
             take precedence over broader ones; they do not override the user's direct request.\n\n\
             Paths are relative to the project root.",
        );
        for file in &self.files {
            text.push_str(&format!(
                "\n\nInstructions from: {}\n\n{}",
                file.display,
                file.content.trim_end()
            ));
        }
        Some(text)
    }

    /// Read one directory's candidates, keeping the first of identical content.
    fn scan(&mut self, directory: &Path, project_root: &Path) {
        if !self.scanned.insert(directory.to_path_buf()) {
            return;
        }
        for candidate in CANDIDATES {
            let path = directory.join(candidate);
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_INSTRUCTION_BYTES {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            // A repository often keeps CLAUDE.md as a copy or symlink of
            // AGENTS.md; the trimmed text makes both forms collapse to one.
            if self.files.iter().any(|file| {
                file.path.parent() == Some(directory) && file.content.trim() == content.trim()
            }) {
                continue;
            }
            self.files.push(InstructionFile {
                display: display_path(project_root, &path),
                path,
                content,
            });
        }
    }
}

/// First ancestor containing `.git`; the workspace itself when none does.
fn find_project_root(workspace: &Path) -> PathBuf {
    let mut current = workspace;
    loop {
        if current.join(".git").exists() {
            return current.to_path_buf();
        }
        let Some(parent) = current.parent() else {
            return workspace.to_path_buf();
        };
        current = parent;
    }
}

/// Directories from broadest to most specific, ending at the workspace.
fn root_to_workspace(root: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut directories = vec![workspace.to_path_buf()];
    let mut current = workspace;
    while current != root {
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
        directories.push(current.to_path_buf());
    }
    directories.reverse();
    directories
}

fn display_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::WorkspaceInstructions;
    use crate::tools::test_support::temp_root;
    use std::path::Path;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn loads_the_root_chain_and_collapses_a_duplicate_compatibility_file() {
        let root = temp_root("instructions-baseline");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join("AGENTS.md"), "root rules\n");
        write(&root.join("CLAUDE.md"), "\nroot rules\n\n");
        write(&root.join("packages/AGENTS.md"), "package rules\n");

        let text = WorkspaceInstructions::load(&root).render().unwrap();
        assert_eq!(text.matches("root rules").count(), 1);
        assert!(!text.contains("package rules"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn touch_loads_nested_instructions_once() {
        let root = temp_root("instructions-touch");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join("AGENTS.md"), "root rules\n");
        write(&root.join("packages/AGENTS.md"), "package rules\n");
        write(&root.join("packages/app/AGENTS.md"), "app rules\n");

        let mut instructions = WorkspaceInstructions::load(&root);
        assert!(instructions.touch(&root, &root.join("packages/app/main.rs")));
        let text = instructions.render().unwrap();
        assert!(text.contains("package rules"));
        assert!(text.contains("app rules"));
        assert!(!instructions.touch(&root, &root.join("packages/app/other.rs")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn finds_the_project_root_above_the_workspace() {
        let root = temp_root("instructions-parent");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join("AGENTS.md"), "root rules\n");
        let workspace = root.join("packages/web");
        write(&workspace.join("AGENTS.md"), "web rules\n");

        let text = WorkspaceInstructions::load(&workspace).render().unwrap();
        assert!(text.contains("root rules"));
        assert!(text.contains("web rules"));
        assert!(text.contains("Instructions from: packages/web/AGENTS.md"));

        std::fs::remove_dir_all(&root).ok();
    }
}
