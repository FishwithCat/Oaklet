use std::process::Stdio;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::{ApprovalRequest, Tool, ToolContext, ToolError, ToolOutput, parse_args};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CAPTURE_BYTES: u64 = 1_000_000;

pub struct RunCommand;

impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in the workspace. The command starts in the workspace root, so relative paths work and no cd to the workspace is needed. Set run_in_background for long builds or servers; it returns a job id immediately. Returns combined stdout and stderr."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run." },
                "run_in_background": { "type": "boolean", "description": "Run in the background and return a job id immediately. Collect output with job_output and stop it with job_kill." }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "run_command already runs in the workspace root; do not prefix commands with cd <workspace>. Commands may need approval depending on Settings, so keep them focused. Long-running commands should use run_in_background."
    }

    fn summary(&self, args: &Value) -> String {
        args.get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn approval(&self, args: &Value) -> Option<ApprovalRequest> {
        let command = args.get("command").and_then(Value::as_str)?.trim();
        if command.is_empty() {
            return None;
        }
        Some(ApprovalRequest {
            tool: self.name().to_string(),
            prompt: "Run this command in the workspace?".into(),
            detail: command.to_string(),
            args: args.clone(),
        })
    }

    fn timeout(&self) -> Option<Duration> {
        Some(COMMAND_TIMEOUT)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { run_command(ctx, args).await.map(ToolOutput::text) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    command: String,
    #[serde(default)]
    run_in_background: bool,
}

async fn run_command(ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
    let args: RunCommandArgs = parse_args(args)?;
    if args.command.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "command must not be empty.".into(),
        ));
    }
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    if args.run_in_background {
        let id = ctx
            .jobs
            .start(ctx.workspace.root(), &args.command)
            .map_err(ToolError::Failed)?;
        return Ok(format!(
            "Started background job {id}. Use job_output with job_id \"{id}\" to read its output, and job_kill to stop it."
        ));
    }

    let mut process = shell_command(&args.command);
    process
        .current_dir(ctx.workspace.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = process
        .spawn()
        .map_err(|error| format!("Could not start command: {error}"))?;
    let stdout = tokio::spawn(read_to_end(child.stdout.take()));
    let stderr = tokio::spawn(read_to_end(child.stderr.take()));

    let status = tokio::select! {
        result = child.wait() => result.map_err(|error| format!("Command failed to run: {error}"))?,
        _ = ctx.cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ToolError::Cancelled);
        }
    };

    let stdout = stdout.await.unwrap_or_default();
    let stderr = stderr.await.unwrap_or_default();
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(&String::from_utf8_lossy(&stdout));
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&String::from_utf8_lossy(&stderr));
    }
    let output = output.trim_end();

    let summary = match status.code() {
        Some(0) => "Command succeeded.".to_string(),
        Some(code) => format!("Exit code {code}."),
        None => "Command terminated by a signal.".to_string(),
    };

    if output.is_empty() {
        Ok(summary)
    } else {
        Ok(format!("{summary}\n{output}"))
    }
}

/// The shell invocation shared by foreground commands and background jobs.
pub(super) fn shell_command(command: &str) -> tokio::process::Command {
    if cfg!(windows) {
        let mut process = tokio::process::Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = tokio::process::Command::new("sh");
        process.args(["-c", command]);
        process
    }
}

async fn read_to_end<R>(reader: Option<R>) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(reader) = reader else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let mut limited = reader.take(MAX_CAPTURE_BYTES);
    let _ = limited.read_to_end(&mut buffer).await;
    buffer
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{RunCommand, Tool, run_command};
    use crate::tools::test_support::context;
    use crate::tools::{CancelHandle, ToolContext, ToolError, Workspace};

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_commands_in_the_workspace_and_reports_status() {
        let (ctx, root, _handle) = context("command");
        let output = run_command(&ctx, json!({ "command": "pwd && echo oaklet" }))
            .await
            .unwrap();
        assert!(output.contains(root.to_str().unwrap()));
        assert!(output.contains("oaklet"));

        let failed = run_command(&ctx, json!({ "command": "exit 3" }))
            .await
            .unwrap();
        assert!(failed.starts_with("Exit code 3."));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn approval_describes_the_command() {
        let request = RunCommand
            .approval(&json!({ "command": "  cargo test  " }))
            .unwrap();
        assert_eq!(request.prompt, "Run this command in the workspace?");
        assert_eq!(request.detail, "cargo test");
        assert!(RunCommand.approval(&json!({})).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancels_a_running_command() {
        let root = crate::tools::test_support::temp_root("cancel");
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (handle, cancel) = CancelHandle::new();
        let ctx = ToolContext {
            workspace,
            cancel,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            run_id: "run-test".into(),
        };

        let task = tokio::spawn(async move {
            let started = Instant::now();
            let result = run_command(&ctx, json!({ "command": "sleep 5" })).await;
            (result, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.cancel();

        let (result, elapsed) = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert!(elapsed < Duration::from_secs(2));
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_background_commands_with_a_job_id() {
        let (ctx, root, _handle) = context("background");
        let output = run_command(
            &ctx,
            json!({ "command": "printf hi", "run_in_background": true }),
        )
        .await
        .unwrap();
        assert!(output.starts_with("Started background job job-"));
        std::fs::remove_dir_all(&root).ok();
    }
}
