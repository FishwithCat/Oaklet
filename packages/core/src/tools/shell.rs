use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::{
    ApprovalRequest, ProcessOutcome, Tool, ToolContext, ToolError, ToolOutput, parse_args,
};
use crate::process_tree::ProcessTree;
use crate::shell_env::ApplyEnvironment;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CAPTURE_BYTES: usize = 1_000_000;
const TRUNCATED: &[u8] = b"[... earlier command output truncated ...]\n";

pub struct RunCommand;

impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        if cfg!(windows) {
            "Run a command with cmd.exe in the workspace. The command starts in the workspace root, so relative paths work and no cd to the workspace is needed. Set run_in_background for long builds or servers; it returns a job id immediately and this chat is told when the job finishes. Returns combined stdout and stderr plus the exit status, and reports a nonzero exit as a failed command."
        } else {
            "Run a shell command with sh in the workspace. The command starts in the workspace root, so relative paths work and no cd to the workspace is needed. Set run_in_background for long builds or servers; it returns a job id immediately and this chat is told when the job finishes. Returns combined stdout and stderr plus the exit status, and reports a nonzero exit or signal as a failed command."
        }
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
        "run_command already runs in the workspace root; do not prefix commands with cd <workspace>. Commands may need approval depending on Settings, so keep them focused. Long-running commands should use run_in_background. Base conclusions on the reported exit status, not on whether output was captured."
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
        Box::pin(async move { run_command(ctx, args).await })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    command: String,
    #[serde(default)]
    run_in_background: bool,
}

async fn run_command(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
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
            .start(
                ctx.workspace.root(),
                &args.command,
                super::jobs::JobOwner {
                    session_id: ctx.session_id.clone(),
                    window: ctx.window.clone(),
                },
            )
            .map_err(ToolError::Failed)?;
        let text = format!(
            "Started background job {id}. Use job_output with job_id \"{id}\" to read its output, and job_kill to stop it."
        );
        return Ok(ToolOutput::text(text).with_outcome(ProcessOutcome::Running { job_id: id }));
    }

    let mut process = shell_command(&args.command);
    process
        .current_dir(crate::paths::process_path(ctx.workspace.root()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);

    let mut child = process
        .spawn()
        .map_err(|error| format!("Could not start command: {error}"))?;
    let mut tree = ProcessTree::new(child.id());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (status, stdout, stderr) = tokio::select! {
        result = async {
            let (status, stdout, stderr) = tokio::join!(
                child.wait(), read_to_end(stdout), read_to_end(stderr),
            );
            Ok::<_, ToolError>((
                status.map_err(|error| format!("Command failed to run: {error}"))?,
                stdout.map_err(|error| format!("Could not read stdout: {error}"))?,
                stderr.map_err(|error| format!("Could not read stderr: {error}"))?,
            ))
        } => result?,
        _ = ctx.cancel.cancelled() => {
            tree.kill();
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ToolError::Cancelled);
        }
    };

    tree.disarm();
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

    let outcome = ProcessOutcome::from_exit(status);
    let summary = match &outcome {
        ProcessOutcome::Succeeded => "Command succeeded.".to_string(),
        ProcessOutcome::Exited { code } => format!("Exit code {code}."),
        ProcessOutcome::Signaled {
            signal,
            signal_name,
        } => format!("Command terminated by signal {signal} ({signal_name})."),
        ProcessOutcome::TimedOut => "Command timed out.".to_string(),
        ProcessOutcome::Cancelled => "Command cancelled.".to_string(),
        ProcessOutcome::Denied => "Command denied.".to_string(),
        ProcessOutcome::Running { job_id } => {
            format!("Background job {job_id} is running.")
        }
    };

    let text = if output.is_empty() {
        summary
    } else {
        format!("{summary}\n{output}")
    };
    Ok(ToolOutput::text(text).with_outcome(outcome))
}

/// The shell invocation shared by foreground commands and background jobs.
pub(super) fn shell_command(command: &str) -> tokio::process::Command {
    let (program, switch) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let mut process = crate::launcher::async_command(program);
    process.args([switch, command]);
    process.apply_environment();
    process
}

async fn read_to_end<R>(reader: Option<R>) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return Ok(Vec::new());
    };
    let mut tail: VecDeque<u8> = VecDeque::new();
    let mut buffer = [0; 8_192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        truncated |= tail.len() + count > MAX_CAPTURE_BYTES;
        let limit = MAX_CAPTURE_BYTES - if truncated { TRUNCATED.len() } else { 0 };
        let excess = (tail.len() + count).saturating_sub(limit);
        tail.drain(..excess);
        tail.extend(&buffer[..count]);
    }
    let mut output = if truncated {
        TRUNCATED.to_vec()
    } else {
        Vec::new()
    };
    output.extend(tail);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{ProcessOutcome, RunCommand, Tool, run_command};
    use crate::tools::test_support::{
        both_streams_and_fail, context, detached_hang, exit_with, print_directory, sleep, succeed,
    };
    use crate::tools::{CancelHandle, ToolContext, ToolError, Workspace};

    #[tokio::test]
    async fn runs_commands_in_the_workspace_and_reports_status() {
        let (ctx, root, _handle) = context("command");
        let output = run_command(
            &ctx,
            json!({ "command": format!("{} && echo oaklet", print_directory()) }),
        )
        .await
        .unwrap();
        assert!(output.contains(&crate::paths::display_path(&root)));
        assert!(output.contains("oaklet"));

        let ok = run_command(&ctx, json!({ "command": succeed() }))
            .await
            .unwrap();
        assert!(ok.starts_with("Command succeeded."));
        assert_eq!(ok.outcome, Some(ProcessOutcome::Succeeded));

        let failed = run_command(&ctx, json!({ "command": exit_with(3) }))
            .await
            .unwrap();
        assert!(failed.starts_with("Exit code 3."));
        assert_eq!(failed.outcome, Some(ProcessOutcome::Exited { code: 3 }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn nonzero_exits_keep_stdout_and_stderr() {
        let (ctx, root, _handle) = context("command-streams");
        let output = run_command(&ctx, json!({ "command": both_streams_and_fail(7) }))
            .await
            .unwrap();
        assert!(output.starts_with("Exit code 7."));
        assert!(output.contains("to-stdout"));
        assert!(output.contains("to-stderr"));
        assert_eq!(output.outcome, Some(ProcessOutcome::Exited { code: 7 }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signals_are_reported_as_termination() {
        let (ctx, root, _handle) = context("command-signal");
        let output = run_command(&ctx, json!({ "command": "kill -TERM $$" }))
            .await
            .unwrap();
        assert!(output.starts_with("Command terminated by signal 15 (SIGTERM)."));
        assert_eq!(
            output.outcome,
            Some(ProcessOutcome::Signaled {
                signal: 15,
                signal_name: "SIGTERM".into(),
            })
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_over_the_cap_does_not_break_the_command_and_keeps_its_tail() {
        let (ctx, root, _handle) = context("command-output-cap");
        let output = run_command(&ctx, json!({
            "command": "head -c 2000000 /dev/zero | tr '\\000' x; status=$?; [ $status -eq 0 ] && printf 'output-tail'; exit $status"
        })).await.unwrap();
        assert!(output.starts_with("Command succeeded."));
        assert!(output.contains("earlier command output truncated"));
        assert!(output.ends_with("output-tail"));
        assert!(output.len() <= super::MAX_CAPTURE_BYTES + "Command succeeded.\n".len());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn output_exactly_at_the_cap_is_not_truncated() {
        let bytes = vec![b'x'; super::MAX_CAPTURE_BYTES];
        assert_eq!(
            super::read_to_end(Some(bytes.as_slice())).await.unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn cancellation_still_works_after_the_shell_exits() {
        let (ctx, root, handle) = context("command-pipe-cancel");
        let task =
            tokio::spawn(
                async move { run_command(&ctx, json!({ "command": detached_hang() })).await },
            );
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
        std::fs::remove_dir_all(root).unwrap();
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

    #[tokio::test]
    async fn cancels_a_running_command() {
        let root = crate::tools::test_support::temp_root("cancel");
        let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
        let (handle, cancel) = CancelHandle::new();
        let ctx = ToolContext {
            workspace,
            cancel,
            access: crate::tools::Access::execute(),
            planning: None,
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(crate::tools::jobs::JobRegistry::new()),
            session_id: "run-test".into(),
            window: "main".into(),
            spill_root: root.join("spill"),
        };

        let task = tokio::spawn(async move {
            let started = Instant::now();
            let result = run_command(&ctx, json!({ "command": sleep(5) })).await;
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

    #[tokio::test]
    async fn starts_background_commands_with_a_job_id() {
        let (ctx, root, _handle) = context("background");
        let output = run_command(
            &ctx,
            json!({ "command": "echo hi", "run_in_background": true }),
        )
        .await
        .unwrap();
        assert!(output.starts_with("Started background job job-"));
        let text = output.text.clone();
        let Some(ProcessOutcome::Running { job_id }) = output.outcome else {
            panic!("a background command should report a running job");
        };
        assert!(text.contains(&job_id));
        std::fs::remove_dir_all(&root).ok();
    }
}
