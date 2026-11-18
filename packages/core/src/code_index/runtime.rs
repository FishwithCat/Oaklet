use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::process::Command;

use super::{CodeIndexConfig, Language, Project};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub version: String,
    pub runtime_versions: Vec<String>,
    pub typescript_sdk: Option<PathBuf>,
}

pub async fn detect_server(
    project: &Project,
    config: &CodeIndexConfig,
) -> Result<Option<ServerCommand>, String> {
    detect_with_path(project, config, std::env::var_os("PATH").as_deref()).await
}

pub(super) async fn detect_with_path(
    project: &Project,
    config: &CodeIndexConfig,
    path: Option<&OsStr>,
) -> Result<Option<ServerCommand>, String> {
    let mut config = config.clone();
    config.validate()?;
    let configured = config
        .servers
        .get(&project.language)
        .cloned()
        .unwrap_or_default();
    if !configured.enabled {
        return Ok(None);
    }
    if let Some(error) = &project.error {
        return Err(error.clone());
    }
    let default = match project.language {
        Language::Rust => "rust-analyzer",
        Language::TypeScript => "typescript-language-server",
    };
    let command = configured.command.as_deref().unwrap_or(default);
    let program = find_program(command, &project.root, path)
        .ok_or_else(|| format!("Code index executable not found: {command}"))?;
    let args = if configured.command.is_some() {
        configured.args
    } else if project.language == Language::TypeScript {
        vec!["--stdio".into()]
    } else {
        Vec::new()
    };
    let mut runtime_versions = Vec::new();
    for name in match project.language {
        Language::Rust => &["rustc", "cargo"][..],
        Language::TypeScript => &["node"][..],
    } {
        let executable = if *name == "node" && program.file_stem() == Some(OsStr::new("node")) {
            program.clone()
        } else {
            find_program(name, &project.root, path).ok_or_else(|| {
                format!(
                    "Required {name} runtime is missing for {}",
                    project.root.display()
                )
            })?
        };
        runtime_versions.push(format!(
            "{name}: {}",
            run(
                &executable,
                &["--version".into()],
                &project.root,
                Duration::from_secs(5)
            )
            .await?
        ));
    }
    let typescript_sdk = if project.language == Language::TypeScript {
        let (sdk, version) = find_typescript(&project.root, &program).ok_or("TypeScript SDK is missing; install TypeScript in the project or alongside the language server.")?;
        runtime_versions.push(format!("typescript: {version}"));
        Some(sdk)
    } else {
        None
    };
    let mut version_args = args.clone();
    version_args.push("--version".into());
    let version = run(
        &program,
        &version_args,
        &project.root,
        Duration::from_secs(5),
    )
    .await?;
    if version.is_empty() {
        return Err(format!("{} returned an empty version", program.display()));
    }
    Ok(Some(ServerCommand {
        program,
        args,
        version,
        runtime_versions,
        typescript_sdk,
    }))
}

fn find_typescript(root: &Path, server: &Path) -> Option<(PathBuf, String)> {
    let server = server.canonicalize().ok()?;
    for ancestor in root.ancestors().chain(server.parent()?.ancestors()) {
        let package = ancestor.join("node_modules/typescript");
        let manifest = package.join("package.json");
        let bytes = std::fs::metadata(&manifest)
            .ok()
            .filter(|metadata| metadata.len() <= 64 * 1024)
            .and_then(|_| std::fs::read(&manifest).ok());
        let Some(bytes) = bytes else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if value["name"] == "typescript"
            && value["version"].is_string()
            && package.join("lib/tsserver.js").is_file()
        {
            return Some((
                package.join("lib").canonicalize().ok()?,
                value["version"].as_str()?.to_string(),
            ));
        }
    }
    None
}

pub(super) fn find_program(command: &str, root: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    let command = Path::new(command);
    let candidates: Vec<_> = if command.is_absolute() || command.components().count() > 1 {
        vec![root.join(command)]
    } else {
        std::env::split_paths(path?)
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(command))
            .collect()
    };
    for candidate in candidates {
        for executable in [candidate.clone(), candidate.with_extension("exe")] {
            #[cfg(not(windows))]
            if executable != candidate {
                continue;
            }
            if !executable.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if !executable
                    .metadata()
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
                {
                    continue;
                }
            }
            // Preserve rustup proxy names; canonicalizing rust-analyzer would select the rustup binary.
            return Some(executable);
        }
    }
    None
}

pub(super) async fn run(
    program: &Path,
    args: &[String],
    root: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_NET_OFFLINE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Cannot start {}: {error}", program.display()))?;
    let stdout = child.stdout.take().ok_or("Missing process stdout")?;
    let stderr = child.stderr.take().ok_or("Missing process stderr")?;
    let work = async {
        let (out, err, status) = tokio::try_join!(
            crate::architecture::read_capped(stdout, 32 * 1024),
            crate::architecture::read_capped(stderr, 32 * 1024),
            async { child.wait().await.map_err(|error| error.to_string()) }
        )?;
        if !status.success() {
            return Err(format!(
                "{} failed: {}",
                program.display(),
                String::from_utf8_lossy(&err).trim()
            ));
        }
        String::from_utf8(out)
            .map(|text| text.trim().to_string())
            .map_err(|error| error.to_string())
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            Err(format!("{} timed out", program.display()))
        }
    }
}
