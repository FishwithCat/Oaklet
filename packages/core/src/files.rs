use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use crate::{SharedState, tools::Workspace, workspace_path};

const MAX_BYTES: u64 = 2_000_000;
static SAVE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    path: String,
    directory: bool,
}

fn workspace(state: &SharedState, expected: &str) -> Result<Workspace, String> {
    let current = workspace_path(state)?;
    if current != Path::new(expected) {
        return Err("The workspace changed. Open the file again.".into());
    }
    Workspace::new(expected)
}

#[tauri::command]
pub async fn files_list(
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<Vec<Entry>, String> {
    let root = self::workspace(&state, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || list(&root, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_read(
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<String, String> {
    let root = self::workspace(&state, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || read(&resolve(&root, &path)?))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_save(
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
    original: String,
    content: String,
) -> Result<(), String> {
    let root = self::workspace(&state, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || save(&root, &path, &original, &content))
        .await
        .map_err(|e| e.to_string())?
}

fn resolve(root: &Workspace, input: &str) -> Result<PathBuf, String> {
    // The shared resolver trims tool input; never silently select a different file.
    if input.trim() != input {
        return Err("Paths with leading or trailing whitespace are not supported.".into());
    }
    root.resolve(input)
}

fn list(root: &Workspace, path: &str) -> Result<Vec<Entry>, String> {
    let directory = resolve(root, path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == ".git" || (!kind.is_dir() && !kind.is_file()) {
            continue;
        }
        entries.push(Entry {
            name,
            path: root.relative(&entry.path()).to_string_lossy().into_owned(),
            directory: kind.is_dir(),
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

fn read(path: &Path) -> Result<String, String> {
    if !fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
        return Err("Choose a regular text file.".into());
    }
    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Choose a regular text file.".into());
    }
    if metadata.len() > MAX_BYTES {
        return Err("Only text files up to 2 MB are supported.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Only text files up to 2 MB are supported.".into());
    }
    if bytes.contains(&0) {
        return Err("Binary files cannot be edited.".into());
    }
    String::from_utf8(bytes).map_err(|_| "Only UTF-8 text files are supported.".into())
}

fn save(root: &Workspace, input: &str, original: &str, content: &str) -> Result<(), String> {
    let _lock = SAVE_LOCK.lock().map_err(|e| e.to_string())?;
    let path = resolve(root, input)?;
    root.ensure_writable(&path)?;
    if content.len() as u64 > MAX_BYTES || content.contains('\0') {
        return Err("Only UTF-8 text files up to 2 MB are supported.".into());
    }
    if read(&path)? != original {
        return Err(
            "File changed on disk. Reload it before saving; your edits have been kept.".into(),
        );
    }
    let permissions = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    if permissions.readonly() {
        return Err("This file is read-only.".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = path.with_file_name(format!(".oaklet-save-{}-{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.set_permissions(permissions)
            .map_err(|e| e.to_string())?;
        file.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // shortcut: external writers can race the final check, use shared locking if coordinated editing is added.
        if read(&path)? != original {
            return Err("File changed on disk. Your edits have been kept.".into());
        }
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| format!("Cannot save file: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_root;

    #[test]
    fn saves_exact_text_and_rejects_conflicts() {
        let dir = temp_root("file-editor");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        let path = dir.join("hello.txt");
        fs::write(&path, "one\r\ntwo\r\n").unwrap();
        save(&root, "hello.txt", "one\r\ntwo\r\n", "你好\r\n").unwrap();
        assert_eq!(read(&path).unwrap(), "你好\r\n");
        assert!(save(&root, "hello.txt", "old", "lost").is_err());
        assert_eq!(read(&path).unwrap(), "你好\r\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_unsafe_and_unsupported_files() {
        let dir = temp_root("file-editor-safety");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        fs::create_dir(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/config"), "git").unwrap();
        assert!(save(&root, ".git/config", "git", "bad").is_err());
        assert!(save(&root, "../outside", "", "bad").is_err());
        assert!(save(&root, ".git/config ", "git", "bad").is_err());
        assert!(resolve(&root, " leading.txt").is_err());
        fs::write(dir.join("binary"), [0, 1, 2]).unwrap();
        assert!(read(&dir.join("binary")).is_err());
        fs::write(dir.join("invalid"), [255]).unwrap();
        assert!(read(&dir.join("invalid")).is_err());
        File::create(dir.join("large"))
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(read(&dir.join("large")).is_err());
        assert!(read(&dir).is_err());
        let entries = list(&root, ".").unwrap();
        assert!(entries.iter().all(|entry| entry.name != ".git"));
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use crate::tools::test_support::temp_root;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn preserves_permissions_and_blocks_external_symlinks() {
        let dir = temp_root("file-permissions");
        let outside = temp_root("file-outside");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        let path = dir.join("run.sh");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        save(&root, "run.sh", "old", "new").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o750
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(save(&root, "run.sh", "new", "bad").is_err());
        fs::write(outside.join("secret"), "secret").unwrap();
        symlink(outside.join("secret"), dir.join("link")).unwrap();
        assert!(resolve(&root, "link").is_err());
        assert!(save(&root, "link", "secret", "bad").is_err());
        assert!(
            list(&root, ".")
                .unwrap()
                .iter()
                .all(|entry| entry.name != "link")
        );
        assert_eq!(
            fs::read_to_string(outside.join("secret")).unwrap(),
            "secret"
        );
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
