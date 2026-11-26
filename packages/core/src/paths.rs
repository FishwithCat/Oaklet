use std::path::{Path, PathBuf};

pub(crate) fn workspace_identity(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(comparable(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

pub(crate) fn display_path(path: &Path) -> String {
    without_verbatim_prefix(&path.to_string_lossy())
}

pub(crate) fn process_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(without_verbatim_prefix(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

pub(crate) fn same_workspace(a: &Path, b: &Path) -> bool {
    workspace_identity(a) == workspace_identity(b)
}

pub(crate) fn is_inside(root: &Path, path: &Path) -> bool {
    #[cfg(windows)]
    {
        comparable_path(path).starts_with(comparable_path(root))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

pub(crate) fn strip_workspace(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix(root) {
        return Some(rest.to_path_buf());
    }
    let depth = root.components().count();
    let mut root_components = root.components();
    let mut path_components = path.components();
    for _ in 0..depth {
        if !same_component(root_components.next()?, path_components.next()?) {
            return None;
        }
    }
    Some(path_components.as_path().to_path_buf())
}

#[cfg(windows)]
fn comparable_path(path: &Path) -> PathBuf {
    PathBuf::from(comparable(&path.to_string_lossy()))
}

#[cfg(windows)]
fn comparable(text: &str) -> String {
    without_verbatim_prefix(text)
        .to_lowercase()
        .replace('\\', "/")
}

fn without_verbatim_prefix(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.to_string()
}

fn same_component(expected: std::path::Component<'_>, actual: std::path::Component<'_>) -> bool {
    #[cfg(windows)]
    {
        expected.as_os_str().to_string_lossy().to_lowercase()
            == actual.as_os_str().to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        expected == actual
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{display_path, same_workspace, strip_workspace, workspace_identity};

    #[test]
    fn identity_is_stable_for_the_same_directory() {
        let root = crate::tools::test_support::temp_root("paths-identity");
        assert_eq!(
            workspace_identity(&root),
            workspace_identity(&root.canonicalize().unwrap())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(windows)]
    #[test]
    fn identity_ignores_case_and_separators_on_windows() {
        assert_eq!(
            workspace_identity(Path::new(r"C:\Work\Project")),
            workspace_identity(Path::new("c:/work/project"))
        );
        assert_eq!(
            workspace_identity(Path::new(r"\\?\C:\Work\Project")),
            workspace_identity(Path::new(r"C:\Work\PROJECT"))
        );
        assert_ne!(
            workspace_identity(Path::new(r"C:\a")),
            workspace_identity(Path::new(r"C:\b"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn display_drops_the_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Work\Project")),
            r"C:\Work\Project"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\project")),
            r"\\server\share\project"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn display_keeps_posix_paths_unchanged() {
        assert_eq!(display_path(Path::new("/work/project")), "/work/project");
    }

    #[test]
    fn workspace_paths_match_themselves() {
        assert!(same_workspace(
            Path::new("/work/project"),
            Path::new("/work/project")
        ));
        assert!(!same_workspace(
            Path::new("/work/project"),
            Path::new("/work/other")
        ));
    }

    #[test]
    fn stripping_the_workspace_keeps_the_remainder() {
        assert_eq!(
            strip_workspace(
                Path::new("/work/project"),
                Path::new("/work/project/src/a.rs")
            ),
            Some(std::path::PathBuf::from("src/a.rs"))
        );
        assert_eq!(
            strip_workspace(Path::new("/work/project"), Path::new("/work/other/a.rs")),
            None
        );
    }
}
