//! Background shell jobs and the model-facing `job_*` controls.
//!
//! A job is a detached child process owned by the app process, not by one run:
//! the model can start a long build or server, keep chatting, and collect the
//! output later. Output is capped and read with a cursor so repeated
//! `job_output` calls return new text instead of the whole log.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::process_tree::ProcessTree;
use crate::store;

use super::shell::{command_label, command_process};
use super::{Cancel, CancelHandle, Tool, ToolContext, ToolError, ToolOutput, parse_args};

const MAX_JOB_OUTPUT: usize = 1_000_000;
/// Finished jobs kept for `job_output` and `job_list`. Running jobs are never
/// evicted; this only stops a long session from retaining every build log.
const MAX_FINISHED_JOBS: usize = 32;
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MAX_MS: u64 = 30_000;
const JOB_OUTPUT_TIMEOUT: Duration = Duration::from_secs(60);

/// The session and window a completion notice returns to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOwner {
    pub session_id: String,
    pub window: String,
}

/// A job that settled without the model asking it to stop, handed to the app's
/// notifier so the owning session can read the result.
#[derive(Clone, Debug)]
pub struct JobSettled {
    pub id: String,
    pub owner: JobOwner,
    pub command: String,
    /// `succeeded` or `failed`.
    pub status: &'static str,
    pub exit: Option<i32>,
    /// Everything the job captured; the notice decides whether it fits.
    pub output: String,
}

type Notifier = Arc<dyn Fn(JobSettled) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobStatus {
    Running,
    Stopping,
    Succeeded,
    Failed,
    Killed,
}

impl JobStatus {
    fn label(self, exit: Option<i32>) -> String {
        match self {
            Self::Running => "running".into(),
            Self::Stopping => "stopping".into(),
            Self::Succeeded => "succeeded".into(),
            Self::Failed => match exit {
                Some(code) => format!("failed (exit {code})"),
                None => "failed".into(),
            },
            Self::Killed => "killed".into(),
        }
    }

    fn running(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
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
    workspace: std::path::PathBuf,
    owner: JobOwner,
    command: String,
    started_at: u64,
    state: Mutex<JobState>,
    /// Set once the model has read the settlement through `job_output`, so the
    /// notice does not repeat a result it already holds.
    collected: AtomicBool,
    cancel: CancelHandle,
    tree: Mutex<ProcessTree>,
}

impl Job {
    fn stop(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.status.running() {
            return false;
        }
        state.status = JobStatus::Stopping;
        self.cancel.cancel();
        self.tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .kill();
        true
    }

    async fn wait_for_tree(&self) {
        loop {
            if !self
                .tree
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .running()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

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
        if !state.status.running() {
            self.collected.store(true, Ordering::SeqCst);
        }
        (chunk, state.status, state.exit)
    }

    fn output_snapshot(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .output
            .clone()
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

    async fn wait_finished(&self) {
        while self.snapshot().0.running() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// App-wide background jobs. The registry survives individual agent runs.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    closed_workspaces: Mutex<HashSet<std::path::PathBuf>>,
    next_id: AtomicU64,
    notifier: Mutex<Option<Notifier>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the app's completion notifier. Tests leave it unset.
    pub fn set_notifier(&self, notifier: Arc<dyn Fn(JobSettled) + Send + Sync>) {
        *self
            .notifier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(notifier);
    }

    /// Start one detached command and return its job id immediately.
    pub fn start(
        &self,
        workspace: &Path,
        command: &str,
        arguments: Option<&[String]>,
        owner: JobOwner,
    ) -> Result<String, String> {
        let closed = self
            .closed_workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if closed.contains(&crate::paths::workspace_identity(workspace)) {
            return Err("The workspace window is closed.".into());
        }
        self.prune();
        let mut process = command_process(command, arguments);
        process
            .current_dir(crate::paths::process_path(workspace))
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
        let (cancel, cancellation) = CancelHandle::new();
        let job = Arc::new(Job {
            id: id.clone(),
            workspace: crate::paths::workspace_identity(workspace),
            owner,
            command: command_label(command, arguments),
            started_at: store::now_ms() as u64,
            state: Mutex::new(JobState {
                status: JobStatus::Running,
                output: String::new(),
                read_pos: 0,
                exit: None,
                truncated: false,
            }),
            collected: AtomicBool::new(false),
            cancel,
            tree: Mutex::new(ProcessTree::new(child.id())),
        });
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), job.clone());

        let notifier = self
            .notifier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        tokio::spawn(run_job(job, child, cancellation, notifier));
        Ok(id)
    }

    /// Mark a workspace as open again after its window is recreated.
    pub fn open_workspace(&self, workspace: &Path) {
        self.closed_workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&crate::paths::workspace_identity(workspace));
    }

    /// Reject new jobs after a workspace window closes.
    pub fn close_workspace(&self, workspace: &Path) {
        self.closed_workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(crate::paths::workspace_identity(workspace));
    }

    fn get(&self, workspace: &Path, id: &str) -> Result<Arc<Job>, String> {
        let job = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| format!("Unknown job id: {id}"))?;
        if job.workspace != crate::paths::workspace_identity(workspace) {
            return Err(format!("Unknown job id: {id}"));
        }
        Ok(job)
    }

    pub fn list(&self, workspace: &Path) -> String {
        self.prune();
        let workspace = crate::paths::workspace_identity(workspace);
        let mut jobs: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|job| job.workspace == workspace)
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

    /// Drop the oldest finished jobs once the registry exceeds its retention
    /// bound. The model can always rerun a command whose log was evicted.
    fn prune(&self) {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut finished: Vec<(u64, String)> = jobs
            .values()
            .filter(|job| !job.snapshot().0.running())
            .map(|job| (job.started_at, job.id.clone()))
            .collect();
        if finished.len() <= MAX_FINISHED_JOBS {
            return;
        }
        let excess = finished.len() - MAX_FINISHED_JOBS;
        finished.sort_by_key(|(started_at, _)| *started_at);
        for (_, id) in finished.drain(..excess) {
            jobs.remove(&id);
        }
    }

    pub async fn output(
        &self,
        workspace: &Path,
        id: &str,
        wait: bool,
        timeout_ms: Option<u64>,
    ) -> Result<String, String> {
        let job = self.get(workspace, id)?;
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

    pub async fn kill(
        &self,
        workspace: &Path,
        id: &str,
        reason: Option<&str>,
    ) -> Result<String, String> {
        let job = self.get(workspace, id)?;
        if !job.stop() {
            return Ok(format!("Job {id} has already finished."));
        }
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        Ok(format!(
            "Requested cancellation of job {id}{reason}. Use job_output to confirm it has stopped."
        ))
    }

    #[cfg(test)]
    fn kill_workspace(&self, workspace: &Path) {
        let workspace = crate::paths::workspace_identity(workspace);
        for job in self.matching_jobs(|job| job.workspace == workspace) {
            job.stop();
        }
    }

    /// Stop running jobs owned by the closing window, including its worktrees.
    pub fn kill_window(&self, window: &str) {
        for job in self.matching_jobs(|job| job.owner.window == window) {
            job.stop();
        }
    }

    /// Stop every running job during app shutdown.
    pub fn kill_all(&self) {
        for job in self.matching_jobs(|_| true) {
            job.stop();
        }
    }

    pub async fn wait_for_exit(&self, workspace: Option<&Path>) {
        let workspace = workspace.map(crate::paths::workspace_identity);
        for job in
            self.matching_jobs(|job| workspace.as_ref().is_none_or(|key| &job.workspace == key))
        {
            job.wait_finished().await;
        }
    }

    pub async fn wait_for_window_exit(&self, window: &str) {
        for job in self.matching_jobs(|job| job.owner.window == window) {
            job.wait_finished().await;
        }
    }

    fn matching_jobs(&self, filter: impl Fn(&Job) -> bool) -> Vec<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|job| filter(job))
            .cloned()
            .collect()
    }
}

async fn run_job(
    job: Arc<Job>,
    mut child: tokio::process::Child,
    cancel: Cancel,
    notifier: Option<Notifier>,
) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (status, exit) = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            job.wait_for_tree().await;
            (JobStatus::Killed, None)
        }
        status = async {
            let (status, _, _) = tokio::join!(
                child.wait(), read_stream(stdout, job.clone()), read_stream(stderr, job.clone()),
            );
            job.wait_for_tree().await;
            status
        } => match status {
            Ok(status) => (
                if status.success() { JobStatus::Succeeded } else { JobStatus::Failed },
                status.code(),
            ),
            Err(_) => (JobStatus::Failed, None),
        },
    };
    job.finish(status, exit);
    // A job the model stopped, or one it already read, needs no notice.
    if let Some(notify) = notifier
        && matches!(status, JobStatus::Succeeded | JobStatus::Failed)
        && !job.collected.load(Ordering::SeqCst)
    {
        notify(JobSettled {
            id: job.id.clone(),
            owner: job.owner.clone(),
            command: job.command.clone(),
            status: if status == JobStatus::Succeeded {
                "succeeded"
            } else {
                "failed"
            },
            exit,
            output: job.output_snapshot(),
        });
    }
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
        Box::pin(async move { Ok(ToolOutput::text(ctx.jobs.list(ctx.workspace.root()))) })
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
                .output(
                    ctx.workspace.root(),
                    &args.job_id,
                    args.wait,
                    args.timeout_ms,
                )
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
                .kill(ctx.workspace.root(), &args.job_id, args.reason.as_deref())
                .await
                .map(ToolOutput::text)
                .map_err(ToolError::Failed)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Job, JobOwner, JobRegistry, JobSettled, JobState, JobStatus, MAX_FINISHED_JOBS};
    use crate::tools::test_support::{detached_hang, detached_hang_silent, sleep, succeed};

    fn owner() -> JobOwner {
        JobOwner {
            session_id: "session-test".into(),
            window: "main".into(),
        }
    }

    #[tokio::test]
    async fn background_jobs_capture_output_and_finish() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, "echo hello", None, owner())
            .expect("job should start");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let output = registry
            .output(&root, &id, true, Some(2_000))
            .await
            .unwrap();
        assert!(output.contains("succeeded"));
        assert!(output.contains("hello"));
        let (status, _, _, _) = registry.jobs.lock().unwrap().get(&id).unwrap().snapshot();
        assert_eq!(status, JobStatus::Succeeded);
    }

    #[tokio::test]
    async fn unknown_job_is_reported() {
        let registry = JobRegistry::new();
        assert!(
            registry
                .output(&std::env::temp_dir(), "nope", false, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn jobs_are_scoped_to_their_workspace() {
        let registry = JobRegistry::new();
        let first = crate::tools::test_support::temp_root("job-workspace-a");
        let second = crate::tools::test_support::temp_root("job-workspace-b");
        let id = registry
            .start(&first, succeed(), None, owner())
            .expect("job should start");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(registry.list(&first).contains(&id));
        assert!(!registry.list(&second).contains(&id));
        assert!(registry.output(&second, &id, false, None).await.is_err());
        assert!(registry.kill(&second, &id, None).await.is_err());

        std::fs::remove_dir_all(first).ok();
        std::fs::remove_dir_all(second).ok();
    }

    #[tokio::test]
    async fn a_closed_workspace_cannot_start_new_jobs() {
        let registry = JobRegistry::new();
        let workspace = crate::tools::test_support::temp_root("job-closed-workspace");
        registry.close_workspace(&workspace);
        assert!(
            registry
                .start(&workspace, succeed(), None, owner())
                .is_err()
        );

        registry.open_workspace(&workspace);
        let id = registry
            .start(&workspace, succeed(), None, owner())
            .expect("job should start");
        assert!(registry.list(&workspace).contains(&id));

        std::fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn finished_jobs_are_evicted_oldest_first() {
        let registry = JobRegistry::new();
        let total = MAX_FINISHED_JOBS + 5;
        {
            let mut jobs = registry.jobs.lock().unwrap();
            for index in 0..total {
                let id = format!("job-{index}");
                jobs.insert(
                    id.clone(),
                    Arc::new(Job {
                        id,
                        workspace: std::env::temp_dir(),
                        owner: owner(),
                        command: "true".into(),
                        started_at: index as u64,
                        state: Mutex::new(JobState {
                            status: JobStatus::Succeeded,
                            output: String::new(),
                            read_pos: 0,
                            exit: Some(0),
                            truncated: false,
                        }),
                        collected: std::sync::atomic::AtomicBool::new(false),
                        cancel: crate::tools::CancelHandle::new().0,
                        tree: Mutex::new(crate::process_tree::ProcessTree::new(None)),
                    }),
                );
            }
        }

        registry.prune();

        let jobs = registry.jobs.lock().unwrap();
        assert_eq!(jobs.len(), MAX_FINISHED_JOBS);
        for index in 0..5 {
            assert!(!jobs.contains_key(&format!("job-{index}")));
        }
        assert!(jobs.contains_key(&format!("job-{}", total - 1)));
    }

    #[tokio::test]
    async fn kill_stops_a_running_job() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, &sleep(30), None, owner())
            .expect("job should start");
        let message = registry.kill(&root, &id, Some("test")).await.unwrap();
        assert!(message.contains("Requested cancellation"));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            registry.wait_for_exit(Some(&root)),
        )
        .await
        .unwrap();
        let output = registry.output(&root, &id, false, None).await.unwrap();
        assert!(output.contains("killed"));
    }

    #[tokio::test]
    async fn stopping_a_job_after_its_shell_exits_kills_the_remaining_process_group() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, detached_hang(), None, owner())
            .unwrap();
        let job = registry.get(&root, &id).unwrap();
        job.wait_for_output(std::time::Duration::from_secs(2)).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(job.snapshot().0, JobStatus::Running);
        let message = registry.kill(&root, &id, None).await.unwrap();
        assert!(message.contains("Requested cancellation"));
        tokio::time::timeout(std::time::Duration::from_secs(2), job.wait_finished())
            .await
            .unwrap();
        assert_eq!(job.snapshot().0, JobStatus::Killed);
        assert!(!job.tree.lock().unwrap().running());
    }

    #[tokio::test]
    async fn closing_a_workspace_stops_descendants_with_redirected_output() {
        let registry = JobRegistry::new();
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, detached_hang_silent(), None, owner())
            .unwrap();
        let job = registry.get(&root, &id).unwrap();
        job.wait_for_output(std::time::Duration::from_secs(2)).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(job.snapshot().0, JobStatus::Running);
        registry.kill_workspace(&root);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            registry.wait_for_exit(Some(&root)),
        )
        .await
        .unwrap();
        assert_eq!(job.snapshot().0, JobStatus::Killed);
        assert!(!job.tree.lock().unwrap().running());
    }

    #[tokio::test]
    async fn a_settled_job_notifies_its_owner_with_output() {
        let registry = JobRegistry::new();
        let seen: Arc<Mutex<Vec<JobSettled>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        registry.set_notifier(Arc::new(move |job| {
            sink.lock().unwrap().push(job);
        }));
        let root = std::env::temp_dir();
        registry
            .start(&root, "echo notice-me", None, owner())
            .unwrap();
        registry.wait_for_exit(Some(&root)).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].owner, owner());
        assert_eq!(seen[0].status, "succeeded");
        assert!(seen[0].output.contains("notice-me"));
    }

    #[tokio::test]
    async fn a_killed_job_does_not_notify() {
        let registry = JobRegistry::new();
        let seen: Arc<Mutex<Vec<JobSettled>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        registry.set_notifier(Arc::new(move |job| {
            sink.lock().unwrap().push(job);
        }));
        let root = std::env::temp_dir();
        let id = registry
            .start(&root, &sleep(30), None, owner())
            .expect("job should start");
        registry.kill(&root, &id, None).await.unwrap();
        registry.wait_for_exit(Some(&root)).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(seen.lock().unwrap().is_empty());
    }
}
