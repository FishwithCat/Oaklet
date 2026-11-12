//! Background shell jobs and the model-facing `job_*` controls.
//!
//! A job is a detached child process owned by the app process, not by one run:
//! the model can start a long build or server, keep chatting, and collect the
//! output later. Output is capped and read with a cursor so repeated
//! `job_output` calls return new text instead of the whole log.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::oneshot;

use crate::store;

use super::shell::shell_command;
use super::{Tool, ToolContext, ToolError, ToolOutput, parse_args};

const MAX_JOB_OUTPUT: usize = 1_000_000;
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MAX_MS: u64 = 30_000;
const JOB_OUTPUT_TIMEOUT: Duration = Duration::from_secs(60);
const KILL_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobStatus {
    Running,
    Succeeded,
    Failed,
    Killed,
}

impl JobStatus {
    fn label(self, exit: Option<i32>) -> String {
        match self {
            Self::Running => "running".into(),
            Self::Succeeded => "succeeded".into(),
            Self::Failed => match exit {
                Some(code) => format!("failed (exit {code})"),
                None => "failed".into(),
            },
            Self::Killed => "killed".into(),
        }
    }

    fn running(self) -> bool {
        self == Self::Running
    }
}

struct JobState {
    status: JobStatus,
    output: String,
    read_pos: usize,
    exit: Option<i32>,
    truncated: bool,
}

struct Job {
    id: String,
    command: String,
    started_at: u64,
    state: Mutex<JobState>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Job {
    fn append(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.truncated {
            return;
        }
        let remaining = MAX_JOB_OUTPUT.saturating_sub(state.output.len());
        let chars = text.chars().count();
        let take = chars.min(remaining);
        state.output.extend(text.chars().take(take));
        if take < chars {
            state
                .output
                .push_str("\n[... job output truncated; rerun with a narrower command ...]\n");
            state.truncated = true;
        }
    }

    fn finish(&self, status: JobStatus, exit: Option<i32>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.status = status;
        state.exit = exit;
    }

    fn take_output(&self) -> (String, JobStatus, Option<i32>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let chunk = state
            .output
            .get(state.read_pos..)
            .unwrap_or_default()
            .to_string();
        state.read_pos = state.output.len();
        (chunk, state.status, state.exit)
    }

    fn snapshot(&self) -> (JobStatus, Option<i32>, usize, bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            state.status,
            state.exit,
            state.output.len(),
            state.read_pos < state.output.len(),
        )
    }

    async fn wait_for_output(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let (status, _, _, unread) = self.snapshot();
            if unread || !status.running() || Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_finished(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.snapshot().0.running() || Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// App-wide background jobs. The registry survives individual agent runs.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    next_id: AtomicU64,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start one detached shell command and return its job id immediately.
    pub fn start(&self, workspace: &Path, command: &str) -> Result<String, String> {
        let mut process = shell_command(command);
        process
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let child = process
            .spawn()
            .map_err(|error| format!("Could not start background command: {error}"))?;

        let id = format!(
            "job-{:x}-{:x}",
            store::now_ms(),
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let (kill_tx, kill_rx) = oneshot::channel();
        let job = Arc::new(Job {
            id: id.clone(),
            command: command.to_string(),
            started_at: store::now_ms() as u64,
            state: Mutex::new(JobState {
                status: JobStatus::Running,
                output: String::new(),
                read_pos: 0,
                exit: None,
                truncated: false,
            }),
            kill: Mutex::new(Some(kill_tx)),
        });
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), job.clone());

        tokio::spawn(run_job(job, child, kill_rx));
        Ok(id)
    }

    fn get(&self, id: &str) -> Result<Arc<Job>, String> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| format!("Unknown job id: {id}"))
    }

    pub fn list(&self) -> String {
        let mut jobs: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect();
        jobs.sort_by_key(|job| job.started_at);
        if jobs.is_empty() {
            return "No background jobs.".into();
        }

        let mut lines = vec!["Background jobs:".to_string()];
        for job in jobs {
            let (status, exit, bytes, _) = job.snapshot();
            lines.push(format!(
                "{}  {}  {} bytes  {}",
                job.id,
                status.label(exit),
                bytes,
                job.command
            ));
        }
        lines.join("\n")
    }

    pub async fn output(
        &self,
        id: &str,
        wait: bool,
        timeout_ms: Option<u64>,
    ) -> Result<String, String> {
        let job = self.get(id)?;
        if wait {
            let timeout =
                Duration::from_millis(timeout_ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MAX_MS));
            job.wait_for_output(timeout).await;
        }
        let (chunk, status, exit) = job.take_output();
        let mut text = format!("Job {id}: {}", status.label(exit));
        if chunk.is_empty() {
            text.push_str(if status.running() {
                "\n(no new output)"
            } else {
                "\n(no output)"
            });
        } else {
            text.push('\n');
            text.push_str(&chunk);
        }
        Ok(text)
    }

    pub async fn kill(&self, id: &str, reason: Option<&str>) -> Result<String, String> {
        let job = self.get(id)?;
        if !job.snapshot().0.running() {
            return Ok(format!("Job {id} has already finished."));
        }
        let sender = job
            .kill
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(sender) = sender else {
            return Ok(format!("Job {id} has already finished."));
        };
        let _ = sender.send(());
        job.wait_finished(KILL_GRACE).await;
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        Ok(format!("Stopped job {id}{reason}."))
    }

    /// Stop every running job during app shutdown.
    pub fn kill_all(&self) {
        let jobs: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect();
        for job in jobs {
            if let Some(sender) = job
                .kill
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = sender.send(());
            }
        }
    }
}

async fn run_job(job: Arc<Job>, mut child: tokio::process::Child, kill: oneshot::Receiver<()>) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_job = job.clone();
    let err_job = job.clone();
    let out = tokio::spawn(async move { read_stream(stdout, out_job).await });
    let err = tokio::spawn(async move { read_stream(stderr, err_job).await });

    let (status, exit) = tokio::select! {
        _ = kill => {
            kill_tree(&mut child).await;
            let _ = child.wait().await;
            (JobStatus::Killed, None)
        }
        status = child.wait() => match status {
            Ok(status) => (
                if status.success() { JobStatus::Succeeded } else { JobStatus::Failed },
                status.code(),
            ),
            Err(_) => (JobStatus::Failed, None),
        },
    };
    let _ = out.await;
    let _ = err.await;
    job.finish(status, exit);
}

/// Kill the whole process group, so a background pipeline or dev server does
/// not leave children holding the output pipes open after its shell exits.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The child was spawned with `process_group(0)`, so its pid is the
        // group id and a negative pid signals every member.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

async fn read_stream<R: AsyncRead + Unpin>(reader: Option<R>, job: Arc<Job>) {
    let Some(mut reader) = reader else {
        return;
    };
    let mut buffer = [0u8; 8_192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => job.append(&buffer[..count]),
        }
    }
}

pub struct JobList;

impl Tool for JobList {
    fn name(&self) -> &str {
        "job_list"
    }

    fn description(&self) -> &'static str {
        "List background jobs with their ids, status, output size, and command."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use job_list to find background jobs started by run_command, job_output to read one, and job_kill to stop one."
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        _args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { Ok(ToolOutput::text(ctx.jobs.list())) })
    }
}

pub struct JobOutput;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobOutputArgs {
    job_id: String,
    #[serde(default)]
    wait: bool,
    timeout_ms: Option<u64>,
}

impl Tool for JobOutput {
    fn name(&self) -> &str {
        "job_output"
    }

    fn description(&self) -> &'static str {
        "Read output from a background job. By default returns output since the previous read. Set wait to block until new output arrives or the job finishes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string", "description": "Job id returned by run_command with run_in_background." },
                "wait": { "type": "boolean", "description": "Block until new output or the job finishes. Defaults to false." },
                "timeout_ms": { "type": "integer", "description": "Maximum wait in milliseconds; capped at 30000. Defaults to 10000." }
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }

    fn summary(&self, args: &Value) -> String {
        args.get("job_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn timeout(&self) -> Option<Duration> {
        Some(JOB_OUTPUT_TIMEOUT)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: JobOutputArgs = parse_args(args)?;
            ctx.jobs
                .output(&args.job_id, args.wait, args.timeout_ms)
                .await
                .map(ToolOutput::text)
                .map_err(ToolError::Failed)
        })
    }
}

pub struct JobKill;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobKillArgs {
    job_id: String,
    reason: Option<String>,
}

impl Tool for JobKill {
    fn name(&self) -> &str {
        "job_kill"
    }

    fn description(&self) -> &'static str {
        "Request cancellation of a running background job."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string", "description": "Job id returned by run_command with run_in_background." },
                "reason": { "type": "string", "description": "Optional short reason for stopping the job." }
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }

    fn summary(&self, args: &Value) -> String {
        args.get("job_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: JobKillArgs = parse_args(args)?;
            ctx.jobs
                .kill(&args.job_id, args.reason.as_deref())
                .await
                .map(ToolOutput::text)
                .map_err(ToolError::Failed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{JobRegistry, JobStatus};

    #[tokio::test]
    async fn background_jobs_capture_output_and_finish() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, "printf 'hello\\n'")
            .expect("job should start");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let output = registry.output(&id, true, Some(2_000)).await.unwrap();
        assert!(output.contains("succeeded"));
        assert!(output.contains("hello"));
        let (status, _, _, _) = registry.jobs.lock().unwrap().get(&id).unwrap().snapshot();
        assert_eq!(status, JobStatus::Succeeded);
    }

    #[tokio::test]
    async fn unknown_job_is_reported() {
        let registry = JobRegistry::new();
        assert!(registry.output("nope", false, None).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_stops_a_running_job() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry.start(&root, "sleep 30").expect("job should start");
        let message = registry.kill(&id, Some("test")).await.unwrap();
        assert!(message.contains("Stopped job"));
        let output = registry.output(&id, false, None).await.unwrap();
        assert!(output.contains("killed"));
    }
}
