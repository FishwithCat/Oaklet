//! Interactive shell sessions for the terminal view.
//!
//! Each session is a real PTY, so the shell handles line editing, job control,
//! signals (`Ctrl+C`), colors, and full-screen programs itself.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// A chunk of raw terminal output, or the shell's final exit code.
#[derive(Debug)]
pub enum Event {
    Output(Vec<u8>),
    Exit { code: Option<u32> },
}

const MAX_IN_FLIGHT: u64 = 64 * 1024;

#[derive(Default)]
struct OutputState {
    sent: u64,
    acknowledged: u64,
    closed: bool,
}

#[derive(Default)]
pub struct OutputFlow {
    state: Mutex<OutputState>,
    ready: Condvar,
}

impl OutputFlow {
    pub fn reserve(&self, bytes: usize) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        while !state.closed && state.sent - state.acknowledged + bytes as u64 > MAX_IN_FLIGHT {
            state = self.ready.wait(state).ok()?;
        }
        if state.closed {
            return None;
        }
        state.sent += bytes as u64;
        Some(state.sent)
    }

    pub fn acknowledge(&self, sequence: u64) {
        if let Ok(mut state) = self.state.lock()
            && sequence > state.acknowledged
            && sequence <= state.sent
        {
            state.acknowledged = sequence;
            self.ready.notify_all();
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            self.ready.notify_all();
        }
    }
}

pub struct Session {
    pub output_flow: Arc<OutputFlow>,
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
        self.output_flow.close();
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

    let (sender, events) = mpsc::sync_channel(8);
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
            output_flow: Arc::new(OutputFlow::default()),
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

    /// Spawns the test shell, or skips the test when the sandbox forbids PTYs.
    ///
    /// Restricted environments (for example the workspace sandbox) deny opening
    /// `/dev/ptmx`, so these integration-style tests cannot run there. Any other
    /// spawn failure is still a real test failure.
    #[cfg(unix)]
    fn spawn_or_skip(
        root: &std::path::Path,
    ) -> Option<(super::Session, std::sync::mpsc::Receiver<Event>)> {
        match spawn_shell(root, 80, 24, Some("/bin/sh")) {
            Ok(shell) => Some(shell),
            Err(error)
                if error.starts_with("Could not open a terminal:")
                    && error.contains("PermissionDenied") =>
            {
                eprintln!("skipping PTY test: {error}");
                let _ = std::fs::remove_dir_all(root);
                None
            }
            Err(error) => panic!("could not start a shell: {error}"),
        }
    }

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

    #[test]
    fn output_waits_for_consumption_and_close_wakes_waiters() {
        use std::sync::{Arc, mpsc};
        let flow = Arc::new(super::OutputFlow::default());
        assert_eq!(
            flow.reserve(super::MAX_IN_FLIGHT as usize),
            Some(super::MAX_IN_FLIGHT)
        );
        let (sent, received) = mpsc::channel();
        let worker_flow = flow.clone();
        let worker = std::thread::spawn(move || {
            sent.send(worker_flow.reserve(1)).unwrap();
        });
        flow.acknowledge(super::MAX_IN_FLIGHT + 1); // Invalid acknowledgements cannot release capacity.
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        flow.acknowledge(1);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(super::MAX_IN_FLIGHT + 1)
        );
        worker.join().unwrap();
        let (sent, received) = mpsc::channel();
        let worker_flow = flow.clone();
        let worker = std::thread::spawn(move || {
            sent.send(worker_flow.reserve(1)).unwrap();
        });
        flow.acknowledge(1); // Duplicate acknowledgement must not release another byte.
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        flow.close();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), None);
        worker.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_shell_in_the_workspace() {
        let root = temp_root("pty");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let Some((mut session, events)) = spawn_or_skip(&root) else {
            return;
        };

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
        let Some((mut session, events)) = spawn_or_skip(&root) else {
            return;
        };

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
        let Some((mut session, events)) = spawn_or_skip(&root) else {
            return;
        };

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
        let Some((mut session, events)) = spawn_or_skip(&root) else {
            return;
        };

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
