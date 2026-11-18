use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use super::{Cancel, CancelHandle, Language, Project, runtime};
use crate::architecture::read_capped;

const TS_SERVER: &str = "6.1.0";
const TYPESCRIPT: &str = "5.9.3";
const PYRIGHT: &str = "1.1.407";
const GOPLS: &str = "v0.17.1";
const JDTLS: &str = "1.63.0";
const JDTLS_URL: &str = "https://download.eclipse.org/jdtls/milestones/1.63.0/jdt-language-server-1.63.0-202610191322.tar.gz";
const JDTLS_SHA256: &str = "b1f2abe0b3cccb462cf64fb4a5587c7d09df60cf8be7748b152cbd89d018bdc4";

fn release(language: Language) -> Option<(&'static str, &'static str)> {
    match language {
        Language::TypeScript => Some(("typescript-language-server", TS_SERVER)),
        Language::Python => Some(("pyright", PYRIGHT)),
        Language::Go => Some(("gopls", GOPLS)),
        Language::Java => Some(("jdtls", JDTLS)),
        Language::Rust => None,
    }
}

pub(super) fn release_dir(root: &Path, language: Language) -> Option<PathBuf> {
    let (name, version) = release(language)?;
    Some(root.join(name).join(version))
}

pub(super) fn available_release(root: &Path, language: Language) -> Option<PathBuf> {
    let current = release_dir(root, language)?;
    if current.is_dir() {
        return Some(current);
    }
    // Only completed version directories are eligible, including after an app upgrade.
    fs::read_dir(current.parent()?)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let version = name.to_str()?.trim_start_matches('v');
            let mut parts = version.split('.');
            let version = [
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            ];
            (parts.next().is_none() && entry.file_type().ok()?.is_dir())
                .then(|| (version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, directory)| directory)
}

pub(super) async fn launch(
    project: &Project,
    directory: &Path,
    path: Option<&OsStr>,
) -> Result<Option<(PathBuf, Vec<String>, PathBuf)>, String> {
    if project.language == Language::Rust {
        // Rustup resolves overrides and rust-toolchain files relative to the project.
        let Some(rustup) = runtime::find_program("rustup", &project.root, path) else {
            return Ok(None);
        };
        let executable = runtime::run(
            &rustup,
            &["which".into(), "rust-analyzer".into()],
            &project.root,
            Duration::from_secs(5),
        )
        .await
        .ok()
        .map(PathBuf::from)
        .filter(|file| file.is_absolute() && file.is_file());
        return Ok(executable.map(|file| (file.clone(), Vec::new(), file)));
    }
    if project.language == Language::Go {
        let program = directory.join(if cfg!(windows) { "gopls.exe" } else { "gopls" });
        return Ok(program
            .is_file()
            .then(|| (program.clone(), Vec::new(), program)));
    }
    let script = match project.language {
        Language::TypeScript => "node_modules/typescript-language-server/lib/cli.mjs",
        Language::Python => "node_modules/pyright/langserver.index.js",
        Language::Java => "bin/jdtls.py",
        _ => return Ok(None),
    };
    let launcher = directory.join(script);
    if !launcher.is_file() {
        return Ok(None);
    }
    let program = if project.language == Language::Java {
        runtime::jdtls_python(&project.root, path)
            .ok_or("Install Python 3 and make it available on PATH.")?
    } else {
        required("node", &project.root, path)?
    };
    let script = launcher
        .to_str()
        .ok_or("Managed server path is not UTF-8")?;
    let mut args = vec![script.into()];
    if project.language != Language::Java {
        args.push("--stdio".into());
    }
    Ok(Some((program, args, launcher)))
}

#[derive(Clone, Debug, Serialize)]
pub enum InstallStatus {
    Installing,
    Verifying,
    Activating,
    Installed,
    Cancelled,
    Failed(String),
}

pub(super) struct Job {
    pub status: InstallStatus,
    pub owner: CancelHandle,
    pub task: tauri::async_runtime::JoinHandle<()>,
}

pub(super) type Jobs = Mutex<BTreeMap<Language, Job>>;

impl InstallStatus {
    pub(super) fn active(&self) -> bool {
        matches!(self, Self::Installing | Self::Verifying | Self::Activating)
    }
}

fn required(name: &str, root: &Path, path: Option<&OsStr>) -> Result<PathBuf, String> {
    runtime::find_program(name, root, path)
        .ok_or_else(|| format!("Install {name} and make it available on PATH, then retry."))
}

fn npm_command(root: &Path, path: Option<&OsStr>) -> Result<Command, String> {
    let node = required("node", root, path)?;
    #[cfg(not(windows))]
    {
        let _ = node;
        Ok(Command::new(required("npm", root, path)?))
    }
    #[cfg(windows)]
    {
        // npm.cmd is a batch wrapper. Invoke npm's JavaScript entry point directly.
        let cli = std::env::split_paths(path.unwrap_or_default())
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join("node_modules/npm/bin/npm-cli.js"))
            .find(|file| file.is_file())
            .ok_or("Install npm with Node.js and make its directory available on PATH.")?;
        let mut command = Command::new(node);
        command.arg(cli);
        Ok(command)
    }
}

struct Stage(PathBuf);

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn install(
    root: &Path,
    project: &Project,
    workspace: &Path,
    path: Option<&OsStr>,
    cancel: &Cancel,
    verifying: impl FnOnce(),
) -> Result<(), String> {
    let language = project.language;
    let (name, _) = release(language).unwrap_or(("rust-analyzer", ""));
    let parent = root.join(name);
    fs::create_dir_all(&parent).map_err(io_error)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join("install.lock"))
        .map_err(io_error)?;
    lock.try_lock()
        .map_err(|error| format!("Could not lock {name} installation: {error}"))?;

    let final_dir = release_dir(root, language);
    let already_installed = final_dir.as_ref().is_some_and(|dir| dir.is_dir());
    let stage_path = parent.join(".installing");
    let stage = Stage(stage_path.clone());
    match fs::remove_dir_all(&stage_path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(io_error(error)),
    }
    fs::create_dir(&stage_path).map_err(io_error)?;
    if !already_installed && language == Language::Java {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Installation cancelled.".into()),
            result = runtime::java_runtime(&project.root, path) => { result?; }
        };
        runtime::jdtls_python(&project.root, path)
            .ok_or("Install Python 3 and make it available on PATH.")?;
        install_jdtls(&stage_path, cancel).await?;
    } else if !already_installed {
        let mut command = match language {
            Language::TypeScript | Language::Python => {
                let mut npm = npm_command(&stage_path, path)?;
                npm.args([
                    "install",
                    "--no-global",
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                    "--no-progress",
                    "--loglevel=error",
                    "--save-exact",
                ])
                .arg("--prefix")
                .arg(&stage_path)
                .arg("--cache")
                .arg(stage_path.join(".npm-cache"));
                if language == Language::TypeScript {
                    npm.args([
                        format!("{name}@{TS_SERVER}"),
                        format!("typescript@{TYPESCRIPT}"),
                    ]);
                } else {
                    npm.arg(format!("{name}@{PYRIGHT}"));
                }
                npm
            }
            Language::Go => {
                let mut go = Command::new(required("go", &stage_path, path)?);
                go.args(["install", &format!("golang.org/x/tools/gopls@{GOPLS}")])
                    .env("GOBIN", &stage_path)
                    .env("GOFLAGS", "")
                    .env("GOTOOLCHAIN", "local")
                    .env("GOWORK", "off");
                go
            }
            Language::Rust => {
                let rustup = required("rustup", &stage_path, path)?;
                let args = ["show".into(), "active-toolchain".into()];
                let active = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("Installation cancelled.".into()),
                    result = runtime::run(
                        &rustup, &args,
                        &project.root, Duration::from_secs(5),
                    ) => result?,
                };
                let toolchain = active
                    .split_whitespace()
                    .next()
                    .filter(|name| !name.starts_with('-'))
                    .ok_or("Rustup has no active installed toolchain for this project.")?;
                let args = [
                    "which".into(),
                    "--toolchain".into(),
                    toolchain.into(),
                    "rustc".into(),
                ];
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("Installation cancelled.".into()),
                    result = runtime::run(&rustup, &args, &stage_path, Duration::from_secs(5)) => {
                        result.map_err(|error| format!("Rust toolchain {toolchain} must already be installed: {error}"))?;
                    }
                }
                let mut command = Command::new(rustup);
                command
                    .args([
                        "component",
                        "add",
                        "rust-analyzer",
                        "--toolchain",
                        toolchain,
                    ])
                    .env("RUSTUP_AUTO_INSTALL", "0");
                command
            }
            Language::Java => unreachable!("Java uses the official Eclipse archive"),
        };
        run(&mut command, &stage_path, cancel).await?;
        let _ = fs::remove_dir_all(stage_path.join(".npm-cache"));
    }

    verifying();
    let candidate = final_dir
        .as_deref()
        .filter(|_| already_installed)
        .unwrap_or(&stage_path);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Installation cancelled.".into()),
        result = probe(project, workspace, candidate, path, cancel) => result?,
    }
    if !already_installed && let Some(destination) = final_dir {
        fs::rename(&stage.0, destination).map_err(io_error)?;
    }
    Ok(())
}

async fn probe(
    project: &Project,
    workspace: &Path,
    candidate: &Path,
    path: Option<&OsStr>,
    cancel: &Cancel,
) -> Result<(), String> {
    if launch(project, candidate, path).await?.is_none() {
        return Err("The installed server executable was not found.".into());
    }
    let command = runtime::detect(project, &Default::default(), path, Some(candidate))
        .await?
        .ok_or("The installed server is disabled.")?;
    match project.language {
        Language::TypeScript if command.version != TS_SERVER => {
            return Err(format!(
                "Expected TypeScript language server {TS_SERVER}, got {}",
                command.version
            ));
        }
        Language::TypeScript => {
            let manifest = candidate.join("node_modules/typescript/package.json");
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(manifest).map_err(io_error)?).map_err(io_error)?;
            if value["version"] != TYPESCRIPT {
                return Err(format!("Expected installed TypeScript SDK {TYPESCRIPT}."));
            }
        }
        Language::Python if command.version != format!("Pyright {PYRIGHT}") => {
            return Err(format!(
                "Expected Pyright {PYRIGHT}, got {}",
                command.version
            ));
        }
        Language::Go if !command.version.split_whitespace().any(|word| word == GOPLS) => {
            return Err(format!("Expected gopls {GOPLS}, got {}", command.version));
        }
        Language::Java if !command.version.starts_with(&format!("JDT LS {JDTLS}.")) => {
            return Err(format!("Expected JDT LS {JDTLS}, got {}", command.version));
        }
        _ => {}
    }
    let mut session = super::lsp::LanguageServerSession::start(
        &project.root,
        workspace,
        project.language,
        &command,
        0,
        cancel,
        Default::default(),
    )
    .await
    .map_err(|error| error.to_string())?;
    session.finish().await;
    Ok(())
}

async fn install_jdtls(directory: &Path, cancel: &Cancel) -> Result<(), String> {
    let archive = directory.join("jdtls.tar.gz");
    let download = async {
        let mut response = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(io_error)?
            .get(JDTLS_URL)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| format!("Could not download Eclipse JDT LS: {error}"))?;
        let mut file = fs::File::create(&archive).map_err(io_error)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        while let Some(chunk) = response.chunk().await.map_err(io_error)? {
            size += chunk.len();
            if size > 128 * 1024 * 1024 {
                return Err("Eclipse JDT LS download exceeded 128 MiB.".into());
            }
            hash.update(&chunk);
            file.write_all(&chunk).map_err(io_error)?;
        }
        if format!("{:x}", hash.finalize()) != JDTLS_SHA256 {
            return Err("The Eclipse JDT LS archive failed SHA-256 verification.".into());
        }
        Ok::<_, String>(())
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Installation cancelled.".into()),
        result = download => result?,
    }
    let destination = directory.to_path_buf();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let file = fs::File::open(&archive).map_err(io_error)?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in tar.entries().map_err(io_error)? {
            if cancel.is_cancelled() {
                return Err("Installation cancelled.".into());
            }
            let mut entry = entry.map_err(io_error)?;
            let kind = entry.header().entry_type();
            if (!kind.is_dir() && !kind.is_file())
                || !entry.unpack_in(&destination).map_err(io_error)?
            {
                return Err("Unexpected path or file type in the Eclipse JDT LS archive.".into());
            }
        }
        fs::remove_file(archive).map_err(io_error)
    })
    .await
    .map_err(io_error)?
}

fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn run(command: &mut Command, directory: &Path, cancel: &Cancel) -> Result<(), String> {
    command
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(io_error)?;
    #[cfg(unix)]
    let mut group = crate::tools::ProcessGroupGuard::new(child.id());
    let stdout = child.stdout.take().ok_or("Missing installer stdout")?;
    let stderr = child.stderr.take().ok_or("Missing installer stderr")?;
    let work = async {
        let (_, err, status) = tokio::try_join!(
            read_capped(stdout, 128 * 1024),
            read_capped(stderr, 128 * 1024),
            async { child.wait().await.map_err(io_error) },
        )?;
        Ok::<_, String>((err, status))
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Installation cancelled.".into()),
        result = tokio::time::timeout(Duration::from_secs(600), work) =>
            result.map_err(|_| "Installation timed out.".to_string()).and_then(|result| result),
    };
    match result {
        Ok((err, status)) => {
            #[cfg(unix)]
            group.disarm();
            if !status.success() {
                return Err(format!(
                    "Installer exited with {status}: {}",
                    String::from_utf8_lossy(&err).trim()
                ));
            }
            Ok(())
        }
        Err(error) => {
            #[cfg(unix)]
            group.kill();
            let _ = child.kill().await;
            #[cfg(unix)]
            group.disarm();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use super::*;
    use crate::code_index::{CodeIndexManager, ServerState};

    #[cfg(unix)]
    #[tokio::test]
    async fn installer_surfaces_failure_and_cancels_running_children() {
        let directory = Stage(crate::tools::test_support::temp_root("installer-process"));
        let (owner, cancel) = CancelHandle::new();
        let mut failing = Command::new("/bin/sh");
        failing.args(["-c", "echo 'network unavailable' >&2; exit 7"]);
        assert!(
            run(&mut failing, &directory.0, &cancel)
                .await
                .unwrap_err()
                .contains("network unavailable")
        );

        let mut waiting = Command::new("/bin/sh");
        waiting.args(["-c", "sleep 30 & echo ready > started; wait"]);
        let cancel_when_started = async {
            while !directory.0.join("started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            owner.cancel();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                run(&mut waiting, &directory.0, &cancel),
                cancel_when_started
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), "Installation cancelled.");
    }

    #[tokio::test]
    async fn missing_installer_prerequisites_are_actionable() {
        let directory = Stage(crate::tools::test_support::temp_root(
            "installer-prerequisites",
        ));
        let path = directory.0.as_os_str();
        for name in ["node", "npm", "go", "rustup"] {
            assert!(
                required(name, &directory.0, Some(path))
                    .unwrap_err()
                    .contains(name)
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_install_removes_staging_and_keeps_the_previous_release() {
        use std::os::unix::fs::PermissionsExt;

        let directory = Stage(crate::tools::test_support::temp_root("failed-installer"));
        let root = directory.0.join("data");
        let version = release_dir(&root, Language::Go).unwrap();
        let parent = version.parent().unwrap();
        let previous = parent.join("v0.16.0");
        fs::create_dir_all(&previous).unwrap();
        let bin = directory.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let go = bin.join("go");
        fs::write(
            &go,
            "#!/bin/sh\necho partial > gopls\necho 'network unavailable' >&2\nexit 7\n",
        )
        .unwrap();
        fs::set_permissions(&go, fs::Permissions::from_mode(0o700)).unwrap();
        let project = Project {
            root: directory.0.clone(),
            language: Language::Go,
            error: None,
        };
        let (_owner, cancel) = CancelHandle::new();
        let error = install(
            &root,
            &project,
            &directory.0,
            Some(bin.as_os_str()),
            &cancel,
            || {
                panic!("A failed transfer must never start verification");
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("network unavailable"), "{error}");
        assert!(!version.exists());
        assert!(!parent.join(".installing").exists());
        assert!(previous.is_dir());
    }

    #[test]
    fn detection_keeps_the_last_release_until_a_new_version_is_published() {
        let directory = Stage(crate::tools::test_support::temp_root("install-versions"));
        let current = release_dir(&directory.0, Language::Python).unwrap();
        let parent = current.parent().unwrap();
        let older = parent.join("1.1.9");
        let previous = parent.join("1.1.99");
        for path in [&older, &previous, &parent.join(".installing")] {
            fs::create_dir_all(path).unwrap();
        }
        assert_eq!(
            available_release(&directory.0, Language::Python),
            Some(previous)
        );
        fs::create_dir(&current).unwrap();
        assert_eq!(
            available_release(&directory.0, Language::Python),
            Some(current)
        );
    }

    // Exercises the same manager as the desktop, with temporary projects and app data.
    #[tokio::test]
    #[ignore = "downloads the pinned release; set OAKLET_INSTALL_TEST to the target language"]
    async fn real_install_retries_both_open_workspaces() {
        let (language, files): (_, &[(&str, &str)]) =
            match std::env::var("OAKLET_INSTALL_TEST").as_deref() {
                Ok("rust") => (
                    Language::Rust,
                    &[
                        (
                            "Cargo.toml",
                            "[package]\nname='install_fixture'\nversion='0.1.0'\nedition='2024'\n",
                        ),
                        ("src/lib.rs", "pub fn greet() {}\n"),
                    ],
                ),
                Ok("typescript") => (
                    Language::TypeScript,
                    &[
                        ("package.json", "{}"),
                        ("greet.ts", "export function greet() {}"),
                    ],
                ),
                Ok("python") => (Language::Python, &[("greet.py", "def greet(): pass\n")]),
                Ok("go") => (
                    Language::Go,
                    &[
                        ("go.mod", "module example.org/fixture\n\ngo 1.23\n"),
                        ("greet.go", "package fixture\nfunc Greet() {}\n"),
                    ],
                ),
                Ok("java") => (Language::Java, &[("Greet.java", "public class Greet {}\n")]),
                _ => panic!("Set OAKLET_INSTALL_TEST to rust, typescript, python, go, or java"),
            };
        let directory = Stage(crate::tools::test_support::temp_root("real-installer"));
        let data = directory.0.join("data");
        let manager = Arc::new(CodeIndexManager::new().with_installation_root(data.clone()));
        let roots = [directory.0.join("a"), directory.0.join("b")];
        for root in &roots {
            for (name, contents) in files {
                let file = root.join(name);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(file, contents).unwrap();
            }
            manager.open_workspace(root, Default::default()).unwrap();
        }

        let work = async {
            let deadline = Instant::now() + Duration::from_secs(60);
            while roots.iter().any(|root| manager.status(root).unwrap().stale) {
                assert!(Instant::now() < deadline, "Discovery timed out");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(
                manager
                    .install_server(&roots[0], &roots[1], language)
                    .is_err()
            );
            manager
                .install_server(&roots[0], &roots[0], language)
                .unwrap();
            manager
                .install_server(&roots[1], &roots[1], language)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(650);
            loop {
                let status = manager.status(&roots[1]).unwrap();
                match &status.installs[&language] {
                    InstallStatus::Installed => break,
                    InstallStatus::Failed(error) => panic!("{error}"),
                    _ => assert!(Instant::now() < deadline, "Installation timed out"),
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if let Some(release) = release_dir(&data, language) {
                assert!(release.is_dir());
                assert!(!release.parent().unwrap().join(".installing").exists());
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            for root in &roots {
                loop {
                    let status = manager.status(root).unwrap();
                    let project = &status.projects[0];
                    assert!(
                        !matches!(project.state, ServerState::Failed(_)),
                        "{status:?}"
                    );
                    if project.state == ServerState::Ready {
                        eprintln!("{language:?} {}: {:?}", root.display(), project.version);
                        break;
                    }
                    assert!(Instant::now() < deadline, "{status:?}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        let result =
            futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(work)).await;
        manager.shutdown().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
