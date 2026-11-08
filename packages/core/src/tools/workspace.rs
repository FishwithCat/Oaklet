use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Canonical workspace root plus the path checks every file tool must share.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(path: &str) -> Result<Self, String> {
        let root = Path::new(path)
            .canonicalize()
            .map_err(|error| format!("Workspace is unavailable: {error}"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a model-supplied path, rejecting `..` escapes and symlinks that
    /// leave the workspace.
    pub fn resolve(&self, input: &str) -> Result<PathBuf, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("A path is required.".into());
        }

        let raw = Path::new(input);
        let candidate = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };

        // Walk up to the deepest entry that exists, canonicalize it, then append
        // the missing names. This resolves symlinks (including broken ones)
        // without trusting `..` segments in the original input.
        let mut ancestor = candidate.as_path();
        let mut missing: Vec<OsString> = Vec::new();
        while ancestor.symlink_metadata().is_err() {
            let Some(parent) = ancestor.parent() else {
                return Err("Invalid path.".into());
            };
            let Some(name) = ancestor.file_name() else {
                return Err("Invalid path.".into());
            };
            missing.push(name.to_os_string());
            ancestor = parent;
        }

        let real = ancestor
            .canonicalize()
            .map_err(|error| format!("Cannot resolve path: {error}"))?;
        if !real.starts_with(&self.root) {
            return Err(format!("Path is outside the workspace: {input}"));
        }

        let mut resolved = real;
        for name in missing.iter().rev() {
            resolved.push(name);
        }
        Ok(resolved)
    }

    /// Workspace-relative form for messages; falls back to the full path.
    pub fn relative<'a>(&'a self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }

    /// Refuse writes anywhere inside a `.git` directory.
    pub fn ensure_writable(&self, path: &Path) -> Result<(), String> {
        let relative = self.relative(path);
        if relative.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        }) {
            return Err("Refusing to write inside .git.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Workspace;
    use crate::tools::test_support::temp_root;

    fn workspace(tag: &str) -> (Workspace, std::path::PathBuf) {
        let root = temp_root(tag);
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        (workspace, root)
    }

    #[test]
    fn resolves_paths_inside_the_workspace() {
        let (workspace, root) = workspace("inside");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        assert_eq!(
            workspace.resolve("src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(workspace.resolve("./src/../src").unwrap(), root.join("src"));
        assert_eq!(
            workspace
                .resolve(root.join("src/main.rs").to_str().unwrap())
                .unwrap(),
            root.join("src/main.rs")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_paths_outside_the_workspace() {
        let (workspace, root) = workspace("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();

        assert!(workspace.resolve("../escape.txt").is_err());
        assert!(workspace.resolve("/etc/passwd").is_err());
        assert!(workspace.resolve("sub/../../escape.txt").is_err());
        assert!(workspace.resolve("..").is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_workspace() {
        let (workspace, root) = workspace("symlink");
        let outside = temp_root("symlink-target");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert!(workspace.resolve("link").is_err());
        assert!(workspace.resolve("link/file.txt").is_err());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
