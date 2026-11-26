use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::path::PathBuf;
use std::process::Command;

/// Build a command for an app-managed process without opening a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let mut command = Command::new(program);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    Command::new(program)
}

pub(crate) fn async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    command(program).into()
}

pub(crate) struct Launch {
    pub(crate) program: OsString,
    pub(crate) batch: Option<OsString>,
}

pub(crate) fn resolve(program: &str) -> Launch {
    #[cfg(windows)]
    {
        let directories = crate::shell_env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<PathBuf>>())
            .unwrap_or_default();
        resolve_in(program, &directories)
    }
    #[cfg(not(windows))]
    {
        Launch {
            program: OsString::from(program),
            batch: None,
        }
    }
}

#[cfg(windows)]
fn resolve_in(program: &str, directories: &[PathBuf]) -> Launch {
    let plain = || Launch {
        program: OsString::from(program),
        batch: None,
    };
    let path = std::path::Path::new(program);
    let candidate = if path.components().count() > 1 {
        find_with_extensions(path)
    } else {
        directories
            .iter()
            .find_map(|directory| find_with_extensions(&directory.join(path)))
    };
    let Some(candidate) = candidate else {
        return plain();
    };
    if is_batch(&candidate) {
        Launch {
            program: comspec(),
            batch: Some(candidate.into_os_string()),
        }
    } else {
        Launch {
            program: candidate.into_os_string(),
            batch: None,
        }
    }
}

#[cfg(windows)]
fn find_with_extensions(base: &std::path::Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    if base.extension().is_some() {
        return None;
    }
    executable_extensions()
        .into_iter()
        .map(|extension| base.with_extension(extension))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn executable_extensions() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    raw.split(';')
        .filter(|extension| !extension.trim().is_empty())
        .map(|extension| extension.trim().trim_start_matches('.').to_string())
        .collect()
}

#[cfg(windows)]
fn is_batch(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(windows)]
fn comspec() -> OsString {
    std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    async fn background_commands_have_no_console() {
        use windows_sys::Win32::System::Console::GetConsoleWindow;

        const CHILD: &str = "OAKLET_TEST_BACKGROUND_CONSOLE";
        const CHECKED: &str = "Oaklet child has no console";
        const ARGS: &[&str] = &[
            "--exact",
            "launcher::tests::background_commands_have_no_console",
            "--nocapture",
        ];
        if std::env::var_os(CHILD).is_some() {
            // SAFETY: GetConsoleWindow takes no arguments and returns a borrowed handle.
            assert!(unsafe { GetConsoleWindow() }.is_null());
            println!("{CHECKED}");
            return;
        }

        let executable = std::env::current_exe().unwrap();
        let sync = command(&executable)
            .args(ARGS)
            .env(CHILD, "1")
            .output()
            .unwrap();
        let asynchronous = async_command(&executable)
            .args(ARGS)
            .env(CHILD, "1")
            .output()
            .await
            .unwrap();
        for output in [sync, asynchronous] {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains(CHECKED),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn keeps_the_configured_program_when_nothing_resolves() {
        let launch = resolve("definitely-not-installed-oaklet");
        assert_eq!(
            launch.program,
            OsString::from("definitely-not-installed-oaklet")
        );
        assert!(launch.batch.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn finds_an_executable_in_a_directory() {
        let root = crate::tools::test_support::temp_root("launch-exe");
        std::fs::write(root.join("oaklet-tool.exe"), "").unwrap();
        let launch = resolve_in("oaklet-tool", std::slice::from_ref(&root));
        assert_eq!(
            launch.program,
            root.join("oaklet-tool.exe").into_os_string()
        );
        assert!(launch.batch.is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(windows)]
    #[test]
    fn runs_batch_files_through_the_command_interpreter() {
        let root = crate::tools::test_support::temp_root("launch-cmd");
        std::fs::write(root.join("oaklet-tool.cmd"), "").unwrap();
        let launch = resolve_in("oaklet-tool", std::slice::from_ref(&root));
        assert_eq!(launch.program, comspec());
        assert_eq!(
            launch.batch,
            Some(root.join("oaklet-tool.cmd").into_os_string())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(not(windows))]
    #[test]
    fn launches_the_program_as_written_elsewhere() {
        assert_eq!(resolve("sh").program, OsString::from("sh"));
    }
}
