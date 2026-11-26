//! LikeC4 helper protocol and subprocess execution.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{Diagnostic, ViewInfo};
use crate::shell_env::ApplyEnvironment;

const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;
const COMPILER_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HelperResponse {
    pub(crate) ok: bool,
    #[serde(default)]
    pub(crate) model: Option<Value>,
    /// One layouted compare view returned for the plan review card.
    #[serde(default)]
    pub(crate) view: Option<Value>,
    #[serde(default)]
    pub(crate) views: Vec<ViewInfo>,
    #[serde(default)]
    pub(crate) diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub(crate) warnings: Vec<Diagnostic>,
    #[serde(default)]
    pub(crate) overlaps: Vec<String>,
    #[serde(default)]
    pub(crate) diff: Option<Value>,
    #[serde(default)]
    pub(crate) draft_error: bool,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

pub(super) async fn run_helper(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<HelperResponse, String> {
    run_helper_inner(
        node,
        helper,
        request,
        HelperCancel::Flag(cancel),
        COMPILER_TIMEOUT,
    )
    .await
}

pub(crate) async fn run_helper_watch(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: crate::tools::Cancel,
) -> Result<HelperResponse, String> {
    run_helper_inner(
        node,
        helper,
        request,
        HelperCancel::Watch(cancel),
        COMPILER_TIMEOUT,
    )
    .await
}

#[cfg(test)]
async fn run_helper_with_timeout(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<HelperResponse, String> {
    run_helper_inner(node, helper, request, HelperCancel::Flag(cancel), timeout).await
}

enum HelperCancel {
    Flag(Arc<AtomicBool>),
    Watch(crate::tools::Cancel),
}

impl HelperCancel {
    async fn cancelled(&self) {
        match self {
            Self::Flag(flag) => wait_cancelled(flag.clone()).await,
            Self::Watch(cancel) => cancel.cancelled().await,
        }
    }
}

async fn run_helper_inner(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: HelperCancel,
    timeout: Duration,
) -> Result<HelperResponse, String> {
    let script = helper.join("src").join("compile.mjs");
    let mut command = crate::launcher::async_command(node);
    command
        .arg(&script)
        .current_dir(crate::paths::process_path(helper))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.apply_environment();
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start the LikeC4 compiler: {error}"))?;
    let mut tree = crate::process_tree::ProcessTree::new(child.id());

    let request = serde_json::to_vec(request)
        .map_err(|error| format!("Could not encode the architecture request: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Could not open the LikeC4 compiler input.".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not read the LikeC4 compiler output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not read the LikeC4 compiler error output.".to_string())?;

    let work = async move {
        stdin
            .write_all(&request)
            .await
            .map_err(|error| format!("Could not send the model to the LikeC4 compiler: {error}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| format!("Could not finish the LikeC4 compiler input: {error}"))?;
        drop(stdin);

        let read_stdout = read_capped(stdout, MAX_HELPER_OUTPUT_BYTES);
        let read_stderr = read_capped(stderr, MAX_HELPER_STDERR_BYTES);
        let wait = async {
            child
                .wait()
                .await
                .map_err(|error| format!("Could not wait for the LikeC4 compiler: {error}"))
        };
        let (out, err, status) = tokio::try_join!(read_stdout, read_stderr, wait)?;
        Ok::<_, String>((out, err, status))
    };

    let outcome = tokio::select! {
        _ = cancel.cancelled() => {
            return Err("Architecture loading was cancelled.".into());
        }
        result = tokio::time::timeout(timeout, work) => {
            match result {
                Ok(result) => result?,
                Err(_) => {
                    return Err(format!(
                        "The LikeC4 compiler did not finish within {} seconds.",
                        timeout.as_secs()
                    ));
                }
            }
        }
    };
    let (stdout, stderr, status) = outcome;
    tree.disarm();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            format!("exit status {status}")
        } else {
            stderr.to_string()
        };
        return Err(format!("The LikeC4 compiler failed: {detail}"));
    }

    let stdout = String::from_utf8(stdout)
        .map_err(|_| "The LikeC4 compiler returned non-UTF-8 output.".to_string())?;
    let response: HelperResponse = serde_json::from_str(&stdout)
        .map_err(|error| format!("Could not read the LikeC4 compiler result: {error}"))?;
    Ok(response)
}

pub(crate) async fn read_capped<R>(reader: R, limit: usize) -> Result<Vec<u8>, String>
where
    R: AsyncReadExt + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|error| format!("Could not read the LikeC4 helper output: {error}"))?;
    if output.len() > limit {
        return Err(format!(
            "The LikeC4 helper output exceeded the {limit}-byte limit."
        ));
    }
    Ok(output)
}

async fn wait_cancelled(cancel: Arc<AtomicBool>) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::architecture::find_node;
    use crate::tools::test_support::temp_root;

    fn fake_helper(tag: &str, script: &str) -> PathBuf {
        let root = temp_root(tag);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/compile.mjs"), script).unwrap();
        root
    }

    #[tokio::test]
    async fn helper_success_is_parsed() {
        let Some(node) = find_node() else {
            return;
        };
        let helper = fake_helper(
            "arch-helper-ok",
            r#"process.stdin.resume(); process.stdin.on("end", () => process.stdout.write(JSON.stringify({ok:true,model:{_stage:"layouted"},views:[{id:"index",title:"Index",kind:"element"}],diagnostics:[]})));"#,
        );
        let response = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(response.ok);
        assert_eq!(response.model.unwrap()["_stage"], "layouted");
        assert_eq!(response.views[0].id, "index");
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_failure_reports_stderr() {
        let Some(node) = find_node() else {
            return;
        };
        let helper = fake_helper(
            "arch-helper-fail",
            r#"process.stderr.write("compiler exploded"); process.exit(2);"#,
        );
        let error = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("compiler exploded"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_times_out_and_is_killed() {
        let Some(node) = find_node() else {
            return;
        };
        let helper = fake_helper("arch-helper-timeout", r#"setInterval(() => {}, 1000);"#);
        let error = run_helper_with_timeout(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
        assert!(error.contains("did not finish"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn helper_cancellation_stops_the_process() {
        let Some(node) = find_node() else {
            return;
        };
        let helper = fake_helper("arch-helper-cancel", r#"setInterval(() => {}, 1000);"#);
        let cancel = Arc::new(AtomicBool::new(true));
        let error = run_helper(
            &node,
            &helper,
            &serde_json::json!({ "version": 1, "sources": { "model.c4": "model { }" } }),
            cancel,
        )
        .await
        .unwrap_err();
        assert!(error.contains("cancelled"));
        fs::remove_dir_all(&helper).ok();
    }

    #[tokio::test]
    async fn compiler_output_is_bounded() {
        let output = vec![b'x'; 32];
        let error = read_capped(output.as_slice(), 16).await.unwrap_err();
        assert!(error.contains("exceeded"));
    }
}
