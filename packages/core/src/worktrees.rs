use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::shell_env::ApplyEnvironment;
use crate::store::{NewUserTurn, Store};
use crate::tools::Cancel;

const POOL: &str = ".agents/worktrees";
const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
pub(crate) static MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionWorktree {
    pub active: Vec<Checkout>,
    pub created: Vec<Checkout>,
    #[serde(default)]
    pub restoring: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<WaitingTurn>,
    #[serde(skip)]
    pub archived: bool,
}

impl SessionWorktree {
    pub fn directory<'a>(&'a self, project: &'a str) -> &'a str {
        self.active.last().map_or(project, |entry| &entry.path)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Checkout {
    pub path: String,
    pub source: String,
    pub common_dir: String,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorktreeRequest {
    pub name: Option<String>,
    pub from: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WaitingTurn {
    pub turn: NewUserTurn,
    pub architecture: bool,
    pub request: Option<WorktreeRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Head {
    pub branch: Option<String>,
    pub commit: Option<String>,
}

struct GitOutput {
    success: bool,
    code: Option<i32>,
    out: String,
}

async fn read_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err("Git output exceeds the worktree limit.".into());
    }
    Ok(bytes)
}

async fn git_output(path: &Path, args: &[&str], allow_failure: bool) -> Result<GitOutput, String> {
    let mut command = Command::new("git");
    command.as_std_mut().apply_environment();
    let hooks_path = format!("core.hooksPath={}", crate::git::NULL_DEVICE);
    command
        .args(["-c", &hooks_path, "-c", "core.fsmonitor=false", "-C"])
        .arg(crate::paths::display_path(path))
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not run Git: {error}"))?;
    let mut tree = crate::process_tree::ProcessTree::new(child.id());
    let stdout = child.stdout.take().expect("piped Git stdout");
    let stderr = child.stderr.take().expect("piped Git stderr");
    tokio::time::timeout(Duration::from_secs(90), async {
        let (out, errors, status) =
            tokio::try_join!(read_output(stdout), read_output(stderr), async {
                child.wait().await.map_err(|error| error.to_string())
            },)?;
        tree.disarm();
        if !status.success() && !allow_failure {
            let message = String::from_utf8_lossy(&errors);
            return Err(if message.trim().is_empty() {
                format!("Git exited with {status}.")
            } else {
                message.trim().to_owned()
            });
        }
        let out = String::from_utf8(out)
            .map_err(|_| "Git returned an unsupported path or reference encoding.")?;
        Ok(GitOutput {
            success: status.success(),
            code: status.code(),
            out,
        })
    })
    .await
    .map_err(|_| "Git worktree operation timed out.".to_owned())?
}

async fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    Ok(git_output(path, args, false).await?.out)
}

fn without_newline(value: &str) -> &str {
    value.strip_suffix('\n').unwrap_or(value)
}

async fn git_path(path: &Path, args: &[&str]) -> Result<PathBuf, String> {
    let output = git(path, args).await?;
    Path::new(without_newline(&output))
        .canonicalize()
        .map_err(|error| error.to_string())
}

async fn root(path: &Path) -> Result<PathBuf, String> {
    git_path(path, &["rev-parse", "--show-toplevel"]).await
}

async fn common(path: &Path) -> Result<PathBuf, String> {
    git_path(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await
}

fn text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "The worktree path must be valid UTF-8.".into())
}

pub(crate) async fn head(path: &Path) -> Result<Head, String> {
    let branch = git(path, &["branch", "--show-current"]).await?;
    let commit = git_output(path, &["rev-parse", "--short", "HEAD"], true).await?;
    Ok(Head {
        branch: (!branch.trim().is_empty()).then(|| branch.trim().to_owned()),
        commit: commit.success.then(|| commit.out.trim().to_owned()),
    })
}

fn relative_name(name: &str) -> Result<&Path, String> {
    let path = Path::new(name);
    if name.is_empty()
        || name.starts_with('-')
        || name.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("Enter a relative Git branch name.".into());
    }
    Ok(path)
}

fn assure_pool_path(source: &Path, name: &Path, prepare: bool) -> Result<PathBuf, String> {
    let target = source.join(POOL).join(name);
    let mut current = source.to_path_buf();
    for part in Path::new(POOL).join(name).components() {
        current.push(part);
        if current == target {
            break;
        }
        match current.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && prepare => {
                match std::fs::create_dir(&current) {
                    Ok(()) if current == source.join(POOL) => {
                        let mut ignore = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(current.join(".gitignore"))
                            .map_err(|error| error.to_string())?;
                        std::io::Write::write_all(&mut ignore, b"*\n")
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(()) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            _ => return Err(format!("Invalid worktree parent: {}", current.display())),
        }
    }
    Ok(target)
}

async fn branch_exists(source: &Path, name: &str) -> Result<bool, String> {
    Ok(git_output(
        source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
        true,
    )
    .await?
    .success)
}

async fn materialize(path: &Path, commit: &str) -> Result<(), String> {
    let keys = git_output(
        path,
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            "^filter\\.",
        ],
        true,
    )
    .await?;
    if !keys.success && keys.code != Some(1) {
        return Err("Could not read Git checkout filter configuration.".into());
    }
    let mut filters = std::collections::BTreeSet::new();
    for key in keys.out.split('\0').filter(|key| !key.is_empty()) {
        if let Some((filter, _)) = key.rsplit_once('.') {
            filters.insert(filter);
        }
    }
    let mut options = Vec::new();
    for filter in filters {
        for (key, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            options.push("-c".to_owned());
            options.push(format!("{filter}.{key}={value}"));
        }
    }
    let mut args: Vec<_> = options.iter().map(String::as_str).collect();
    args.extend([
        "read-tree",
        "--reset",
        "-u",
        "--no-recurse-submodules",
        commit,
    ]);
    git(path, &args).await?;
    Ok(())
}

async fn registered(source: &Path, target: &Path) -> Result<Option<Vec<String>>, String> {
    let listing = git(source, &["worktree", "list", "--porcelain", "-z"]).await?;
    Ok(listing.split("\0\0").find_map(|entry| {
        let mut fields = entry.split('\0');
        let path = fields.next()?.strip_prefix("worktree ")?;
        crate::paths::same_workspace(Path::new(path), target)
            .then(|| fields.map(str::to_owned).collect())
    }))
}

fn stopped(cancel: &Cancel) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("Worktree operation stopped.".into())
    } else {
        Ok(())
    }
}

pub(crate) async fn create(
    directory: &Path,
    request: WorktreeRequest,
    fallback: &str,
    cancel: &Cancel,
    phase: impl Fn(&str),
) -> Result<Checkout, String> {
    let branch = request.name.as_deref().unwrap_or(fallback);
    let name = relative_name(branch)?;
    let source = root(directory).await?;
    let common_dir = common(&source).await?;
    if !common_dir.starts_with(directory) {
        return Err(
            "This checkout's shared Git directory is outside the writable worktree root.".into(),
        );
    }
    let revision = request.from.as_deref().unwrap_or("HEAD");
    phase("prepare");
    git(
        &source,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .await
    .map_err(|_| "Invalid Git branch name.".to_owned())?;
    if branch == "HEAD" || branch_exists(&source, branch).await? {
        return Err(format!("Branch already exists or is reserved: {branch}"));
    }
    let commit = git(
        &source,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )
    .await?;
    let commit = commit.trim();
    stopped(cancel)?;
    let target = assure_pool_path(&source, name, true)?;
    let path = text(&target)?.to_owned();
    let argument = crate::paths::display_path(&target);
    std::fs::create_dir(&target)
        .map_err(|error| format!("Could not allocate worktree {path}: {error}"))?;
    let result = async {
        git(
            &source,
            &[
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                branch,
                "--",
                &argument,
                commit,
            ],
        )
        .await?;
        stopped(cancel)?;
        phase("checkout");
        tokio::select! {
            _ = cancel.cancelled() => return Err("Worktree operation stopped.".into()),
            result = materialize(&target, commit) => result?,
        }
        stopped(cancel)?;
        if root(&target).await? != target || common(&target).await? != common_dir {
            return Err("Could not verify the new worktree's checkout.".into());
        }
        Ok(Checkout {
            path: path.clone(),
            source: text(&source)?.to_owned(),
            common_dir: text(&common_dir)?.to_owned(),
            branch: Some(branch.to_owned()),
            base_commit: Some(commit.to_owned()),
        })
    }
    .await;
    result.map_err(|error: String| {
        format!("{error} Any checkout and branch created at {path} have been retained.")
    })
}

pub(crate) async fn enter(directory: &Path, name: &str) -> Result<Option<Checkout>, String> {
    let source = root(directory).await?;
    let target = assure_pool_path(&source, relative_name(name)?, false)?;
    if !target.is_dir() {
        return Ok(None);
    }
    if target.canonicalize().ok().as_deref() != Some(&target)
        || registered(&source, &target).await?.is_none()
        || root(&target).await? != target
    {
        return Ok(None);
    }
    let common_dir = common(&source).await?;
    if common(&target).await? != common_dir {
        return Ok(None);
    }
    Ok(Some(Checkout {
        path: text(&target)?.to_owned(),
        source: text(&source)?.to_owned(),
        common_dir: text(&common_dir)?.to_owned(),
        branch: head(&target).await?.branch,
        base_commit: None,
    }))
}

async fn recorded_target(entry: &Checkout) -> Result<PathBuf, String> {
    let source = Path::new(&entry.source);
    let path = Path::new(&entry.path);
    if root(source).await? != source || common(source).await? != Path::new(&entry.common_dir) {
        return Err("The worktree's source repository changed.".into());
    }
    let name = path
        .strip_prefix(source.join(POOL))
        .map_err(|_| "The recorded worktree is outside its pool.")?;
    assure_pool_path(source, relative_name(text(name)?)?, false)
}

pub(crate) async fn restore(
    entry: &Checkout,
    cancel: &Cancel,
    phase: impl Fn(&str),
) -> Result<bool, String> {
    let source = Path::new(&entry.source);
    let target = recorded_target(entry).await?;
    if target.symlink_metadata().is_ok() {
        return Err("The recorded worktree path is already occupied.".into());
    }
    let branch = entry
        .branch
        .as_deref()
        .ok_or("The branch for this worktree is unavailable.")?;
    let branch_ref = format!("refs/heads/{branch}");
    let exists = branch_exists(source, branch).await?;
    let base = if exists {
        &branch_ref
    } else {
        entry
            .base_commit
            .as_ref()
            .ok_or("The original worktree branch no longer exists.")?
    };
    phase("prepare");
    let commit = git(
        source,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )
    .await?;
    let commit = commit.trim();
    let argument = crate::paths::display_path(&target);
    if let Some(fields) = registered(source, &target).await? {
        if fields
            .iter()
            .any(|field| field.starts_with("locked") || field == "bare")
        {
            return Err("The recorded worktree is locked.".into());
        }
        stopped(cancel)?;
        git(source, &["worktree", "remove", "--", &argument]).await?;
    }
    stopped(cancel)?;
    let name = target
        .strip_prefix(source.join(POOL))
        .map_err(|_| "The recorded worktree is outside its pool.")?;
    assure_pool_path(source, name, true)?;
    std::fs::create_dir(&target).map_err(|error| error.to_string())?;
    if exists {
        git(
            source,
            &["worktree", "add", "--no-checkout", "--", &argument, branch],
        )
        .await?;
    } else {
        git(
            source,
            &[
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                branch,
                "--",
                &argument,
                commit,
            ],
        )
        .await?;
    }
    phase("checkout");
    tokio::select! {
        _ = cancel.cancelled() => Err(format!("Restore stopped. The checkout and branch at {} were retained.", entry.path)),
        result = materialize(&target, "HEAD") => result,
    }?;
    validate(entry).await?;
    Ok(!exists)
}

pub(crate) async fn validate(entry: &Checkout) -> Result<(), String> {
    let target = Path::new(&entry.path);
    if !target.is_dir()
        || target.canonicalize().ok().as_deref() != Some(target)
        || root(target).await? != target
        || common(target).await? != Path::new(&entry.common_dir)
    {
        return Err(format!("Worktree is unavailable: {}", entry.path));
    }
    Ok(())
}

pub(crate) async fn cleanup(store: &Store, entry: &Checkout) -> Result<bool, String> {
    let source = Path::new(&entry.source);
    let path = recorded_target(entry).await?;
    let Some(branch) = &entry.branch else {
        return Ok(false);
    };
    if !path.is_dir() || store.worktree_referenced(&path)? {
        return Ok(false);
    }
    validate(entry).await?;
    let Some(fields) = registered(source, &path).await? else {
        return Ok(false);
    };
    if !fields.contains(&format!("branch refs/heads/{branch}"))
        || fields
            .iter()
            .any(|field| field.starts_with("locked") || field.starts_with("prunable"))
        || head(&path).await?.branch.as_ref() != Some(branch)
        || !git(
            &path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )
        .await?
        .is_empty()
    {
        return Ok(false);
    }
    let modules = git(
        &path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "modules",
        ],
    )
    .await?;
    if Path::new(without_newline(&modules)).exists() || store.worktree_referenced(&path)? {
        return Ok(false);
    }
    git(source, &["worktree", "remove", "--", &entry.path]).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{CancelHandle, test_support::temp_root};

    #[tokio::test]
    async fn keeps_the_source_intact_and_restores_the_original_checkout() {
        let source = temp_root("session-worktree");
        git(&source, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(source.join("tracked"), "committed").unwrap();
        git(&source, &["add", "tracked"]).await.unwrap();
        git(
            &source,
            &[
                "-c",
                "user.name=Oaklet",
                "-c",
                "user.email=oaklet@example.test",
                "commit",
                "-m",
                "Initial",
            ],
        )
        .await
        .unwrap();
        std::fs::write(source.join("tracked"), "dirty").unwrap();
        std::fs::write(source.join("untracked"), "only in the source").unwrap();
        let (_handle, cancel) = CancelHandle::new();
        let request = WorktreeRequest {
            name: Some("feature/login".into()),
            from: None,
        };
        let entry = create(&source, request.clone(), "", &cancel, |_| {})
            .await
            .unwrap();
        let target = Path::new(&entry.path);
        assert_eq!(target, source.join(".agents/worktrees/feature/login"));
        assert_eq!(
            std::fs::read_to_string(target.join("tracked")).unwrap(),
            "committed"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("tracked")).unwrap(),
            "dirty"
        );
        assert!(!target.join("untracked").exists());
        assert_eq!(head(&source).await.unwrap().branch.as_deref(), Some("main"));
        assert_eq!(
            head(target).await.unwrap().branch.as_deref(),
            Some("feature/login")
        );
        assert_eq!(
            enter(&source, "feature/login").await.unwrap().unwrap().path,
            entry.path
        );
        assert!(enter(&source, "../feature/login").await.is_err());
        let conflict = create(&source, request, "", &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(conflict.contains("Branch already exists"));

        std::fs::remove_dir_all(target).unwrap();
        assert!(!restore(&entry, &cancel, |_| {}).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("tracked")).unwrap(),
            "committed"
        );

        let store = Store::in_memory().unwrap();
        std::fs::write(target.join("untracked"), "keep this").unwrap();
        assert!(!cleanup(&store, &entry).await.unwrap());
        std::fs::remove_file(target.join("untracked")).unwrap();
        assert!(cleanup(&store, &entry).await.unwrap());
        assert!(branch_exists(&source, "feature/login").await.unwrap());

        git(&source, &["branch", "-D", "feature/login"])
            .await
            .unwrap();
        assert!(restore(&entry, &cancel, |_| {}).await.unwrap());
        assert_eq!(
            head(target).await.unwrap().branch.as_deref(),
            Some("feature/login")
        );
        std::fs::remove_dir_all(source).unwrap();
    }
}
