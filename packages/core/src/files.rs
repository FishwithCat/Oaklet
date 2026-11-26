use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{State, WebviewWindow};

use crate::syntax::{self, Grammar, Outline};
use crate::{SharedState, tools::Workspace};

const MAX_BYTES: u64 = 2_000_000;
/// Matches the composer's text attachment cap; larger drops keep their path.
const MAX_DROP_TEXT_BYTES: u64 = 200_000;
const MAX_FIND_RESULTS: usize = 20_000;
static FILE_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    path: String,
    directory: bool,
}

fn workspace(
    state: &SharedState,
    window: &WebviewWindow,
    expected: &str,
) -> Result<Workspace, String> {
    crate::chat::worktree::view_directory(state, window.label(), Some(expected))?;
    Workspace::new(expected)
}

#[tauri::command]
pub async fn files_list(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<Vec<Entry>, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || list(&root, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_find(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
) -> Result<Vec<String>, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || find(&root))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_read(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<String, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    let target = resolve(&root, &path)?;
    let label = crate::paths::relative_text(&root.relative(&target));
    tauri::async_runtime::spawn_blocking(move || {
        read(&target).map_err(|error| format!("{label}: {error}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn files_outline(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
    content: String,
) -> Result<Outline, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    let target = resolve(&root, &path)?;
    let grammar = Grammar::for_path(&target.to_string_lossy()).ok_or("Unsupported source file.")?;
    if content.len() > syntax::MAX_SOURCE_BYTES {
        return Err("Only source files up to 2 MB are supported.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        syntax::outline(grammar, &content, 1_000, || {
            std::time::Instant::now() >= deadline
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// A file dropped from the OS. Small files become composer attachments; larger
/// or binary files keep their absolute path for the model to inspect.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DroppedFile {
    Image {
        name: String,
        bytes: u64,
        data_url: String,
    },
    Text {
        name: String,
        bytes: u64,
        text: String,
    },
    Path {
        path: String,
    },
}

#[tauri::command]
pub async fn files_read_dropped(path: String) -> Result<DroppedFile, String> {
    tauri::async_runtime::spawn_blocking(move || read_dropped(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_read_image(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<String, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    let target = resolve(&root, &path)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (bytes, _) = crate::tools::image::read_image(&target)?;
        crate::tools::image::data_url(&bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_save(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
    original: String,
    content: String,
) -> Result<(), String> {
    let root = self::workspace(&state, &window, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || save(&root, &path, &original, &content))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_rename(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
    name: String,
) -> Result<String, String> {
    let root = self::workspace(&state, &window, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || rename(&root, &path, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn files_delete(
    window: WebviewWindow,
    state: State<'_, SharedState>,
    workspace: String,
    path: String,
) -> Result<(), String> {
    let root = self::workspace(&state, &window, &workspace)?;
    tauri::async_runtime::spawn_blocking(move || delete(&root, &path))
        .await
        .map_err(|e| e.to_string())?
}

fn entry_name(name: &str) -> bool {
    Path::new(name).file_name().is_some_and(|part| part == name)
        && !name.contains('\0')
        && (!cfg!(windows) || !name.contains(['\\', ':']))
}

fn mutation_target(root: &Workspace, input: &str) -> Result<(PathBuf, fs::FileType), String> {
    if !input.split('/').all(entry_name) {
        return Err("Choose a workspace file or folder.".into());
    }
    let path = root.root().join(input);
    root.ensure_writable(&path)?;
    let kind = path
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .file_type();
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    if (!kind.is_dir() && !kind.is_file()) || canonical != path || canonical == root.root() {
        return Err("Choose a workspace file or folder without symbolic links.".into());
    }
    Ok((canonical, kind))
}

fn rename(root: &Workspace, input: &str, name: &str) -> Result<String, String> {
    if !entry_name(name) || name.trim() != name || (cfg!(windows) && name.ends_with('.')) {
        return Err("Enter a valid file or folder name.".into());
    }
    let _lock = FILE_WRITE_LOCK.lock().map_err(|e| e.to_string())?;
    let (path, _) = mutation_target(root, input)?;
    let destination = path.with_file_name(name);
    root.ensure_writable(&destination)?;
    if path != destination {
        rename_exclusive(&path, &destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!("An item named \"{name}\" already exists in this folder.")
            } else {
                format!("Cannot rename item: {error}")
            }
        })?;
    }
    Ok(crate::paths::relative_text(&root.relative(&destination)))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: Both paths are live, NUL-terminated strings for the duration of the call.
    let result = unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::renameatx_np(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileW;

    let from: Vec<_> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: Both paths are live, NUL-terminated UTF-16 strings.
    if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn delete(root: &Workspace, input: &str) -> Result<(), String> {
    let _lock = FILE_WRITE_LOCK.lock().map_err(|e| e.to_string())?;
    let (path, kind) = mutation_target(root, input)?;
    if kind.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|e| format!("Cannot delete item: {e}"))
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
            path: crate::paths::relative_text(&root.relative(&entry.path())),
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

/// Every visible file in the workspace for the editor's quick-open list.
fn find(root: &Workspace) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    for result in crate::tools::search::walker(root.root(), true).build() {
        let Ok(entry) = result else {
            continue;
        };
        if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry.path().to_str().is_none() {
            continue;
        }
        files.push(crate::paths::relative_text(&root.relative(entry.path())));
        if files.len() == MAX_FIND_RESULTS {
            break;
        }
    }
    files.sort();
    Ok(files)
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

fn read_dropped(path: &Path) -> Result<DroppedFile, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Cannot read file: {error}"))?;
    if !metadata.is_file() {
        return Err("Only files can be dropped into the chat.".into());
    }
    let Some(absolute) = path.to_str() else {
        return Err("Only files with UTF-8 paths can be dropped into the chat.".into());
    };
    let bytes = metadata.len();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    if bytes <= crate::tools::image::MAX_IMAGE_BYTES && looks_like_image(path) {
        let (data, _) = crate::tools::image::read_image(path)?;
        return Ok(DroppedFile::Image {
            name,
            bytes,
            data_url: crate::tools::image::data_url(&data)?,
        });
    }
    if bytes <= MAX_DROP_TEXT_BYTES
        && let Ok(text) = read(path)
    {
        return Ok(DroppedFile::Text { name, bytes, text });
    }
    Ok(DroppedFile::Path {
        path: absolute.to_string(),
    })
}

fn looks_like_image(path: &Path) -> bool {
    let mut head = [0u8; 12];
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    crate::tools::image::image_mime(&head[..read]).is_some()
}

fn save(root: &Workspace, input: &str, original: &str, content: &str) -> Result<(), String> {
    let _lock = FILE_WRITE_LOCK.lock().map_err(|e| e.to_string())?;
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
    fn renames_and_deletes_files_and_folders_without_overwriting() {
        let dir = temp_root("file-mutations");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        fs::create_dir(dir.join("docs")).unwrap();
        fs::create_dir(dir.join("existing")).unwrap();
        fs::write(dir.join("docs/old.md"), "keep").unwrap();
        fs::write(dir.join("docs/taken.md"), "other").unwrap();

        assert!(rename(&root, "docs/old.md", "taken.md").is_err());
        assert!(rename(&root, "docs/old.md", "../new.md").is_err());
        assert!(rename(&root, "docs/old.md", ".Git").is_err());
        assert!(rename(&root, "docs", "existing").is_err());
        assert!(rename(&root, ".", "elsewhere").is_err());
        assert!(delete(&root, ".").is_err());
        assert_eq!(read(&dir.join("docs/taken.md")).unwrap(), "other");
        assert_eq!(
            rename(&root, "docs/old.md", "new.md").unwrap(),
            "docs/new.md"
        );
        assert_eq!(rename(&root, "docs", "notes").unwrap(), "notes");
        assert_eq!(read(&dir.join("notes/new.md")).unwrap(), "keep");

        delete(&root, "notes/new.md").unwrap();
        assert!(!dir.join("notes/new.md").exists());
        delete(&root, "notes").unwrap();
        assert!(!dir.join("notes").exists());
        assert!(dir.join("existing").is_dir());
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

    #[test]
    fn drops_attachments_and_falls_back_to_paths() {
        let dir = temp_root("dropped-files");
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        assert!(matches!(
            read_dropped(&dir.join("notes.txt")),
            Ok(DroppedFile::Text { .. })
        ));

        fs::write(dir.join("shot.png"), b"\x89PNG\r\n\x1a\npixel").unwrap();
        assert!(matches!(
            read_dropped(&dir.join("shot.png")),
            Ok(DroppedFile::Image { .. })
        ));

        fs::write(dir.join("binary.bin"), [0, 1, 2]).unwrap();
        assert!(matches!(
            read_dropped(&dir.join("binary.bin")),
            Ok(DroppedFile::Path { .. })
        ));

        File::create(dir.join("large.log"))
            .unwrap()
            .set_len(MAX_DROP_TEXT_BYTES + 1)
            .unwrap();
        assert!(matches!(
            read_dropped(&dir.join("large.log")),
            Ok(DroppedFile::Path { .. })
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dropped_files_serialize_in_the_composer_shape() {
        let image = serde_json::to_value(DroppedFile::Image {
            name: "shot.png".into(),
            bytes: 3,
            data_url: "data:image/png;base64,AAA".into(),
        })
        .unwrap();
        assert_eq!(image["kind"], "image");
        assert_eq!(image["dataUrl"], "data:image/png;base64,AAA");

        let text = serde_json::to_value(DroppedFile::Text {
            name: "notes.md".into(),
            bytes: 5,
            text: "hello".into(),
        })
        .unwrap();
        assert_eq!(text["kind"], "text");

        let path = serde_json::to_value(DroppedFile::Path {
            path: "/tmp/large.bin".into(),
        })
        .unwrap();
        assert_eq!(path["kind"], "path");
        assert_eq!(path["path"], "/tmp/large.bin");
    }

    #[test]
    fn finds_workspace_files_and_honors_ignore_rules() {
        let dir = temp_root("file-find");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        fs::create_dir(dir.join("src")).unwrap();
        fs::create_dir(dir.join(".git")).unwrap();
        fs::write(dir.join("src/main.rs"), "").unwrap();
        fs::write(dir.join(".git/config"), "").unwrap();
        fs::write(dir.join(".gitignore"), "secret.txt\n").unwrap();
        fs::write(dir.join("secret.txt"), "").unwrap();

        let files = find(&root).unwrap();

        assert_eq!(
            files,
            vec![".gitignore".to_string(), "src/main.rs".to_string()]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use crate::tools::test_support::temp_root;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn refuses_link_entries_and_leaves_linked_contents_alone() {
        let dir = temp_root("file-mutation-links");
        let outside = temp_root("file-mutation-outside");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        fs::create_dir(dir.join("docs")).unwrap();
        fs::write(dir.join("docs/keep.txt"), "local").unwrap();
        fs::write(outside.join("keep.txt"), "external").unwrap();
        symlink(dir.join("docs"), dir.join("alias")).unwrap();
        symlink(&outside, dir.join("outside")).unwrap();
        symlink(&outside, dir.join("docs/contained")).unwrap();

        assert!(delete(&root, "alias").is_err());
        assert!(delete(&root, "alias/keep.txt").is_err());
        assert!(rename(&root, "outside/keep.txt", "lost.txt").is_err());
        assert!(rename(&root, "docs/keep.txt", "contained").is_err());
        assert_eq!(read(&dir.join("docs/keep.txt")).unwrap(), "local");
        delete(&root, "docs").unwrap();
        assert_eq!(read(&outside.join("keep.txt")).unwrap(), "external");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

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

    #[test]
    fn keeps_a_backslash_in_a_name_apart_from_a_folder() {
        let dir = temp_root("file-backslash");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        fs::create_dir(dir.join("a")).unwrap();
        fs::write(dir.join("a\\b.txt"), "backslash").unwrap();
        fs::write(dir.join("a/b.txt"), "nested").unwrap();

        let top = list(&root, ".").unwrap();
        assert!(
            top.iter()
                .any(|entry| entry.path == "a\\b.txt" && !entry.directory)
        );
        assert!(top.iter().any(|entry| entry.path == "a" && entry.directory));

        let folder = list(&root, "a").unwrap();
        assert!(folder.iter().any(|entry| entry.path == "a/b.txt"));

        assert_eq!(
            read(&resolve(&root, "a\\b.txt").unwrap()).unwrap(),
            "backslash"
        );
        assert_eq!(read(&resolve(&root, "a/b.txt").unwrap()).unwrap(), "nested");

        let found = find(&root).unwrap();
        assert!(found.contains(&"a\\b.txt".to_string()));
        assert!(found.contains(&"a/b.txt".to_string()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn skips_names_and_drops_paths_that_are_not_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = temp_root("file-non-utf8");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        let invalid = OsStr::from_bytes(b"bad-\xff.txt");
        if fs::write(dir.join(invalid), "x").is_err() {
            eprintln!("skipping non-UTF-8 name test: this filesystem rejects the name");
            fs::remove_dir_all(dir).ok();
            return;
        }
        fs::write(dir.join("good.txt"), "x").unwrap();

        let entries = list(&root, ".").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "good.txt");
        assert_eq!(find(&root).unwrap(), vec!["good.txt".to_string()]);
        assert!(read_dropped(&dir.join(invalid)).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::tools::test_support::temp_root;

    #[test]
    fn replaces_the_file_and_leaves_no_staging_file() {
        let dir = temp_root("file-save-windows");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        let path = dir.join("notes.txt");
        fs::write(&path, "one\r\ntwo\r\n").unwrap();

        save(&root, "notes.txt", "one\r\ntwo\r\n", "你好\r\ntwo\r\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "你好\r\ntwo\r\n");
        let staging = fs::read_dir(&dir)
            .unwrap()
            .filter(|entry| {
                entry.as_ref().is_ok_and(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".oaklet-save-")
                })
            })
            .count();
        assert_eq!(staging, 0);
        assert!(save(&root, "notes.txt", "stale", "changed").is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_a_read_only_file() {
        let dir = temp_root("file-save-readonly");
        let root = Workspace::new(dir.to_str().unwrap()).unwrap();
        let path = dir.join("locked.txt");
        fs::write(&path, "locked").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        assert!(save(&root, "locked.txt", "locked", "changed").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "locked");

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        fs::remove_dir_all(&dir).ok();
    }
}
