//! Interactive shell sessions for the terminal view.
//!
//! Each session is a real PTY, so the shell handles line editing, job control,
//! signals (`Ctrl+C`), colors, and full-screen programs itself.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// A chunk of raw terminal output, or the shell's final exit code.
#[derive(Debug)]
pub enum Event {
    Output(Vec<u8>),
    Exit { code: Option<u32> },
}

pub struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Session {
    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .write_all(data)
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("Could not write to the terminal: {error}"))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(2),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("Could not resize the terminal: {error}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The shell is the PTY session leader; killing it hangs up its jobs.
        let _ = self.killer.kill();
    }
}

pub fn spawn(workspace: &Path, cols: u16, rows: u16) -> Result<(Session, Receiver<Event>), String> {
    spawn_shell(workspace, cols, rows, None)
}

fn spawn_shell(
    workspace: &Path,
    cols: u16,
    rows: u16,
    program: Option<&str>,
) -> Result<(Session, Receiver<Event>), String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("Could not open a terminal: {error}"))?;

    // No explicit program means the user's login shell, like a normal terminal.
    let mut command = match program {
        Some(program) => CommandBuilder::new(program),
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(workspace);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("Could not start a shell: {error}"))?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("Could not read from the terminal: {error}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("Could not write to the terminal: {error}"))?;

    let (sender, events) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if sender.send(Event::Output(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
        // The PTY closed, so the shell is exiting; reap it to report the code.
        let code = child.wait().ok().map(|status| status.exit_code());
        let _ = sender.send(Event::Exit { code });
    });

    Ok((
        Session {
            master: pair.master,
            writer,
            killer,
        },
        events,
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Event, spawn_shell};
    use crate::tools::test_support::temp_root;

    fn wait_for(events: &std::sync::mpsc::Receiver<Event>, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Output(bytes)) => {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                    if output.contains(needle) {
                        return output;
                    }
                }
                Ok(Event::Exit { .. }) | Err(_) => break,
            }
        }
        panic!("did not see {needle:?} in {output:?}");
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_shell_in_the_workspace() {
        let root = temp_root("pty");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let (mut session, events) = spawn_shell(&root, 80, 24, Some("/bin/sh")).unwrap();

        session.write(b"echo $((6*7))\r").unwrap();
        wait_for(&events, "42");

        session.write(b"cd sub\r").unwrap();
        session.write(b"pwd\r").unwrap();
        wait_for(&events, &format!("{}/sub", root.display()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ctrl_c_interrupts_the_foreground_process() {
        let root = temp_root("pty-interrupt");
        let (mut session, events) = spawn_shell(&root, 80, 24, Some("/bin/sh")).unwrap();

        session.write(b"sleep 30\r").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.write(b"\x03").unwrap();
        session.write(b"echo $((6*7))\r").unwrap();
        wait_for(&events, "42");

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn reports_the_shell_exit() {
        let root = temp_root("pty-exit");
        let (mut session, events) = spawn_shell(&root, 80, 24, Some("/bin/sh")).unwrap();

        session.write(b"exit\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Event::Exit { code }) = events.recv_timeout(Duration::from_millis(50)) {
                assert_eq!(code, Some(0));
                std::fs::remove_dir_all(&root).ok();
                return;
            }
        }
        panic!("shell did not report an exit");
    }

    #[cfg(unix)]
    #[test]
    fn closing_the_session_stops_the_shell() {
        let root = temp_root("pty-close");
        let (mut session, events) = spawn_shell(&root, 80, 24, Some("/bin/sh")).unwrap();

        session.write(b"sleep 30\r").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(session);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Event::Exit { .. }) = events.recv_timeout(Duration::from_millis(50)) {
                std::fs::remove_dir_all(&root).ok();
                return;
            }
        }
        panic!("shell kept running after the session closed");
    }
}
