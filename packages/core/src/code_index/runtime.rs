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
    pub python_interpreter: Option<PathBuf>,
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
        Language::Python => "pyright-langserver",
        Language::Go => "gopls",
        Language::Java => "jdtls",
    };
    let command = configured.command.as_deref().unwrap_or(default);
    let program = find_program(command, &project.root, path)
        .ok_or_else(|| format!("Code index executable not found: {command}"))?;
    let args = if configured.command.is_some() {
        configured.args
    } else if matches!(project.language, Language::TypeScript | Language::Python) {
        vec!["--stdio".into()]
    } else {
        Vec::new()
    };
    let mut runtime_versions = Vec::new();
    for name in match project.language {
        Language::Rust => &["rustc", "cargo"][..],
        Language::TypeScript => &["node"][..],
        Language::Python => &["node"][..],
        Language::Go => &["go"][..],
        Language::Java => &["java"][..],
    } {
        let executable = if *name == "node" && program.file_stem() == Some(OsStr::new("node")) {
            program.clone()
        } else if *name == "java" {
            std::env::var_os("JAVA_HOME")
                .and_then(|home| {
                    find_program(
                        &Path::new(&home).join("bin/java").to_string_lossy(),
                        &project.root,
                        path,
                    )
                })
                .or_else(|| find_program(name, &project.root, path))
                .ok_or("Required Java runtime is missing")?
        } else {
            find_program(name, &project.root, path).ok_or_else(|| {
                format!(
                    "Required {name} runtime is missing for {}",
                    project.root.display()
                )
            })?
        };
        let flag = match *name {
            "go" => "version",
            "java" => "-version",
            _ => "--version",
        };
        let version = run(
            &executable,
            &[flag.into()],
            &project.root,
            Duration::from_secs(5),
        )
        .await?;
        if *name == "java"
            && version
                .split('"')
                .nth(1)
                .and_then(|version| version.split('.').next()?.parse::<u32>().ok())
                .is_none_or(|major| major < 21)
        {
            return Err(format!("JDT LS requires JDK 21 or newer; found: {version}"));
        }
        runtime_versions.push(format!("{name}: {version}"));
    }
    let python_interpreter = if project.language == Language::Python
        || project.language == Language::Java && configured.command.is_none()
    {
        let python = if project.language == Language::Java {
            find_program("python3", &project.root, path)
        } else {
            python_program(&project.root, path)
        }.ok_or("Required Python interpreter is missing; configure the project environment or add python3 to PATH.")?;
        runtime_versions.push(format!(
            "python: {} ({})",
            run(
                &python,
                &["--version".into()],
                &project.root,
                Duration::from_secs(5)
            )
            .await?,
            python.display()
        ));
        Some(python)
    } else {
        None
    };
    let typescript_sdk = if project.language == Language::TypeScript {
        let (sdk, version) = find_typescript(&project.root, &program).ok_or("TypeScript SDK is missing; install TypeScript in the project or alongside the language server.")?;
        runtime_versions.push(format!("typescript: {version}"));
        Some(sdk)
    } else {
        None
    };
    let version = match project.language {
        Language::Python => installed_version(&program, "pyright")
            .unwrap_or_else(|| "Pyright (version unknown; custom launcher)".into()),
        Language::Java => installed_version(&program, "jdtls")
            .unwrap_or_else(|| "JDT LS (version unknown; custom launcher)".into()),
        _ => {
            let mut version_args = args.clone();
            version_args.push(
                if project.language == Language::Go {
                    "version"
                } else {
                    "--version"
                }
                .into(),
            );
            run(
                &program,
                &version_args,
                &project.root,
                Duration::from_secs(5),
            )
            .await?
        }
    };
    if matches!(project.language, Language::Python | Language::Java) {
        let metadata = program.metadata().map_err(|error| error.to_string())?;
        runtime_versions.push(format!(
            "launcher: {} {:?}",
            metadata.len(),
            metadata.modified().ok()
        ));
    }
    if version.is_empty() {
        return Err(format!("{} returned an empty version", program.display()));
    }
    Ok(Some(ServerCommand {
        program,
        args,
        version,
        runtime_versions,
        typescript_sdk,
        python_interpreter,
    }))
}

fn python_program(root: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    for environment in [".venv", "venv"] {
        let executable = if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        };
        if let Some(program) = find_program(&format!("./{environment}/{executable}"), root, path) {
            return Some(program);
        }
    }
    find_program("python3", root, path).or_else(|| find_program("python", root, path))
}

// Pyright and JDT LS launchers do not provide a portable --version probe.
fn installed_version(program: &Path, package: &str) -> Option<String> {
    let resolved = program.canonicalize().ok()?;
    for ancestor in resolved.parent()?.ancestors().take(5) {
        if package == "pyright" {
            let manifest = ancestor.join("package.json");
            if std::fs::metadata(&manifest).is_ok_and(|metadata| metadata.len() <= 64 * 1024)
                && let Ok(bytes) = std::fs::read(&manifest)
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                && value["name"] == package
                && let Some(version) = value["version"].as_str()
            {
                return Some(format!("Pyright {version}"));
            }
        } else if let Ok(entries) = std::fs::read_dir(ancestor.join("plugins")) {
            for entry in entries.take(4096).flatten() {
                let name = entry.file_name();
                if let Some(version) = name
                    .to_str()?
                    .strip_prefix("org.eclipse.jdt.ls.core_")
                    .and_then(|name| name.strip_suffix(".jar"))
                {
                    return Some(format!("JDT LS {version}"));
                }
            }
        }
    }
    None
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
        .env("GOTOOLCHAIN", "local")
        .env("GOPROXY", "off")
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
        String::from_utf8(if out.is_empty() { err } else { out })
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
