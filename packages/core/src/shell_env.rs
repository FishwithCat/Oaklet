//! Rebuild the process environment from the user's login shell.
//!
//! Finder and Dock launches give a macOS app launchd's minimal environment, so
//! variables exported by `~/.zprofile` or `~/.zshrc` never reach it. Read the
//! login shell once at startup and merge its environment back into this process
//! so every child (agent shell, terminal, git hook, MCP server) inherits it.

#[cfg(unix)]
mod imp {
    use std::ffi::{OsStr, OsString};
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const DELIMITER: &str = "_OAKLET_SHELL_ENV_DELIMITER_";
    const DUMP: &str = "printf '\\0%s\\0' '_OAKLET_SHELL_ENV_DELIMITER_'; command env -0 || exit; printf '\\0%s\\0' '_OAKLET_SHELL_ENV_DELIMITER_'; exit";
    const TIMEOUT: Duration = Duration::from_secs(10);
    const MAX_OUTPUT: usize = 4 * 1024 * 1024;
    /// Variables describing the probe shell, plus launcher-owned configuration
    /// the shell must not overwrite.
    const SKIPPED: [&str; 7] = [
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
        "DISABLE_AUTO_UPDATE",
        "ZSH_TMUX_AUTOSTARTED",
        "ZSH_TMUX_AUTOSTART",
    ];

    /// Read the login shell environment and apply it to this process.
    ///
    /// Returns a diagnostic when every candidate fails; the app then keeps the
    /// inherited environment. Must run before Tauri starts threads or children.
    pub(crate) fn hydrate() -> Option<String> {
        let mut failures = Vec::new();
        for shell in candidates() {
            match read_environment(&shell, TIMEOUT) {
                Ok(environment) => {
                    apply(&environment);
                    return None;
                }
                Err(reason) => failures.push(format!("{}: {reason}", shell.display())),
            }
        }
        Some(format!(
            "could not read the login shell environment ({}); tool commands will use the inherited environment",
            failures.join("; ")
        ))
    }

    /// Account shell first (like Terminal), then `$SHELL`, then system shells.
    fn candidates() -> Vec<PathBuf> {
        let mut shells = Vec::new();
        if let Some(shell) = account_shell() {
            shells.push(shell);
        }
        if let Some(shell) = std::env::var_os("SHELL") {
            let shell = PathBuf::from(shell);
            if shell.is_absolute() {
                shells.push(shell);
            }
        }
        shells.extend(["/bin/zsh", "/bin/bash", "/bin/sh"].map(PathBuf::from));

        let mut seen = std::collections::BTreeSet::new();
        shells
            .into_iter()
            .filter(|shell| shell.is_file() && seen.insert(shell.clone()))
            .collect()
    }

    fn account_shell() -> Option<PathBuf> {
        // SAFETY: getpwuid returns a pointer to a static entry. Copy it now;
        // hydration runs before Tauri starts any thread that could call it.
        unsafe {
            let entry = libc::getpwuid(libc::getuid());
            if entry.is_null() || (*entry).pw_shell.is_null() {
                return None;
            }
            let shell = std::ffi::CStr::from_ptr((*entry).pw_shell).to_bytes();
            if shell.is_empty() {
                None
            } else {
                Some(PathBuf::from(OsStr::from_bytes(shell)))
            }
        }
    }

    fn read_environment(
        shell: &Path,
        timeout: Duration,
    ) -> Result<Vec<(OsString, OsString)>, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"));

        let mut command = Command::new(shell);
        command
            .args(["-ilc", DUMP])
            .current_dir(home)
            // Keep common rc hooks from waiting on a prompt or an update check.
            .env("DISABLE_AUTO_UPDATE", "true")
            .env("ZSH_TMUX_AUTOSTARTED", "true")
            .env("ZSH_TMUX_AUTOSTART", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.process_group(0);

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "stdout was not captured".to_string())?;

        // Reap the shell even when startup files leave a background process
        // holding stdout open.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        let output = Arc::new(Mutex::new(Vec::new()));
        let reader_output = output.clone();
        let (ready, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut buffer = [0_u8; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if let Ok(mut output) = reader_output.lock()
                            && output.len() < MAX_OUTPUT
                        {
                            output.extend_from_slice(&buffer[..read]);
                        }
                        // Keep draining after the caller has its answer so rc
                        // background children do not fail with EPIPE.
                        let _ = ready.send(());
                    }
                }
            }
        });

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(kill_group(pid, "timed out"));
            }
            match chunks.recv_timeout(remaining) {
                Ok(()) => {
                    let output = snapshot(&output)?;
                    if output.len() >= MAX_OUTPUT {
                        return Err(kill_group(pid, "printed more than 4 MiB"));
                    }
                    if complete(&output) {
                        return parse(&output)
                            .ok_or_else(|| "environment envelope was unparsed".to_string());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(kill_group(pid, "timed out"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let output = snapshot(&output)?;
                    return if output.len() >= MAX_OUTPUT {
                        Err(kill_group(pid, "printed more than 4 MiB"))
                    } else {
                        parse(&output)
                            .ok_or_else(|| "environment envelope was unparsed".to_string())
                    };
                }
            }
        }
    }

    fn snapshot(output: &Mutex<Vec<u8>>) -> Result<Vec<u8>, String> {
        let output = output
            .lock()
            .map_err(|_| "environment buffer was poisoned".to_string())?;
        Ok(output.clone())
    }

    /// Kill the shell's process group and return the failure reason.
    fn kill_group(pid: u32, reason: &str) -> String {
        // SAFETY: the child was spawned as a process-group leader, so a negative
        // pid targets it and any children it started.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        reason.to_string()
    }

    fn complete(output: &[u8]) -> bool {
        let mut markers = output
            .split(|byte| *byte == 0)
            .filter(|part| *part == DELIMITER.as_bytes());
        markers.next().is_some() && markers.next().is_some()
    }

    /// Parse the NUL-delimited dump between the two markers. Anything rc files
    /// print outside the envelope is ignored.
    fn parse(output: &[u8]) -> Option<Vec<(OsString, OsString)>> {
        let parts: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
        let first = parts
            .iter()
            .position(|part| *part == DELIMITER.as_bytes())?;
        let last = parts
            .iter()
            .rposition(|part| *part == DELIMITER.as_bytes())?;
        if first == last {
            return None;
        }

        let mut environment = Vec::new();
        for entry in &parts[first + 1..last] {
            let Some(split) = entry.iter().position(|byte| *byte == b'=') else {
                continue;
            };
            if split == 0 {
                continue;
            }
            environment.push((
                OsStr::from_bytes(&entry[..split]).to_os_string(),
                OsStr::from_bytes(&entry[split + 1..]).to_os_string(),
            ));
        }
        Some(environment)
    }

    fn apply(environment: &[(OsString, OsString)]) {
        for (key, value) in environment {
            if skip(key) {
                continue;
            }
            // SAFETY: `run` calls hydrate before Tauri creates any thread or
            // child, so no other thread can read the environment concurrently.
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }

    fn skip(key: &OsStr) -> bool {
        let key = key.as_bytes();
        SKIPPED.iter().any(|skipped| key == skipped.as_bytes()) || key.starts_with(b"OAKLET_")
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        use super::{DELIMITER, complete, parse, read_environment, skip};
        use crate::tools::test_support::temp_root;

        fn shell(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
            let path = root.join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\n[ \"$1\" = -ilc ] || exit 64\n{body}\nexec /bin/sh -c \"$2\"\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn value<'a>(
            environment: &'a [(std::ffi::OsString, std::ffi::OsString)],
            key: &str,
        ) -> &'a str {
            environment
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.to_str().unwrap())
                .unwrap()
        }

        #[test]
        fn parses_only_the_delimited_environment() {
            let output = format!(
                "banner\0{DELIMITER}\0PATH=/bin\0MULTI=a\nb=still-value\0=ignored\0NOEQUALS\0\0{DELIMITER}\0trailing"
            );
            let environment = parse(output.as_bytes()).unwrap();
            assert_eq!(environment.len(), 2);
            assert_eq!(value(&environment, "PATH"), "/bin");
            assert_eq!(value(&environment, "MULTI"), "a\nb=still-value");
        }

        #[test]
        fn rejects_output_without_a_complete_envelope() {
            assert!(!complete(b"banner\0"));
            assert!(parse(b"banner\0").is_none());
            assert!(parse(format!("\0{DELIMITER}\0PATH=/bin\0").as_bytes()).is_none());
        }

        #[test]
        fn reads_rc_exports_through_the_login_shell() {
            let root = temp_root("shell-env-read");
            let login = shell(
                &root,
                "login",
                "echo banner\nexport PATH=\"/login/bin:$PATH\"\nexport MULTI='a\nb'",
            );
            let environment = read_environment(&login, Duration::from_secs(5)).unwrap();
            assert!(value(&environment, "PATH").starts_with("/login/bin:"));
            assert_eq!(value(&environment, "MULTI"), "a\nb");
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn times_out_and_returns_the_reason() {
            let root = temp_root("shell-env-timeout");
            let login = shell(&root, "slow", "sleep 30");
            let started = Instant::now();
            let error = read_environment(&login, Duration::from_millis(200)).unwrap_err();
            assert_eq!(error, "timed out");
            assert!(started.elapsed() < Duration::from_secs(5));
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn timeout_kills_the_shells_process_group() {
            let root = temp_root("shell-env-timeout-group");
            let pid_file = root.join("sleep.pid");
            let login = shell(
                &root,
                "slow",
                &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
            );
            assert_eq!(
                read_environment(&login, Duration::from_millis(500)).unwrap_err(),
                "timed out"
            );

            let pid: i32 = fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                // SAFETY: signal 0 only checks whether the process still exists.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "background child survived the timeout"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn skips_probe_and_launcher_variables() {
            assert!(skip(std::ffi::OsStr::new("PWD")));
            assert!(skip(std::ffi::OsStr::new("OAKLET_NODE")));
            assert!(!skip(std::ffi::OsStr::new("PATH")));
        }
    }
}

#[cfg(unix)]
pub(crate) use imp::hydrate;

#[cfg(not(unix))]
pub(crate) fn hydrate() -> Option<String> {
    None
}
