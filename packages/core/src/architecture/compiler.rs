//! LikeC4 helper protocol and subprocess execution.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::{Diagnostic, ViewInfo};

const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;
const COMPILER_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HelperResponse {
    pub(super) ok: bool,
    #[serde(default)]
    pub(super) model: Option<Value>,
    #[serde(default)]
    pub(super) views: Vec<ViewInfo>,
    #[serde(default)]
    pub(super) diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub(super) error: Option<String>,
}

pub(super) async fn run_helper(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<HelperResponse, String> {
    run_helper_with_timeout(node, helper, request, cancel, COMPILER_TIMEOUT).await
}

async fn run_helper_with_timeout(
    node: &Path,
    helper: &Path,
    request: &Value,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<HelperResponse, String> {
    let script = helper.join("src").join("compile.mjs");
    let mut child = Command::new(node)
        .arg(&script)
        .current_dir(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Could not start the LikeC4 compiler: {error}"))?;

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
        _ = wait_cancelled(cancel) => {
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
