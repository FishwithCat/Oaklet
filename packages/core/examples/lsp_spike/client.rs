use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_lsp::{MainLoop, ResponseError, ServerSocket, router::Router};
use lsp_types::{notification, request};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Bound the frame before async-lsp allocates its Content-Length-sized buffer.
pub async fn forward_bounded(
    input: impl AsyncRead + Unpin,
    mut output: impl tokio::io::AsyncWrite + Unpin,
) -> std::io::Result<()> {
    let mut input = BufReader::new(input);
    loop {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() == MAX_HEADER_BYTES {
                return Err(std::io::Error::other("LSP header exceeds 8 KiB"));
            }
            match input.read_u8().await {
                Ok(byte) => header.push(byte),
                Err(error)
                    if header.is_empty() && error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        let header_text = std::str::from_utf8(&header).map_err(std::io::Error::other)?;
        let mut length = None;
        for line in header_text.split("\r\n").filter(|line| !line.is_empty()) {
            let (name, value) = line
                .split_once(": ")
                .ok_or_else(|| std::io::Error::other("Invalid LSP header"))?;
            if name.eq_ignore_ascii_case("Content-Length") {
                if length.is_some() {
                    return Err(std::io::Error::other("Duplicate Content-Length"));
                }
                length = Some(value.parse::<usize>().map_err(std::io::Error::other)?);
            }
        }
        let length = length.ok_or_else(|| std::io::Error::other("Missing Content-Length"))?;
        if length > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::other("LSP message exceeds 8 MiB"));
        }
        output.write_all(&header).await?;
        let copied = tokio::io::copy(&mut (&mut input).take(length as u64), &mut output).await?;
        if copied != length as u64 {
            return Err(std::io::Error::other("Truncated LSP body"));
        }
        output.flush().await?;
    }
}

pub struct Client {
    pub server: ServerSocket,
    pub notifications: Arc<Mutex<BTreeMap<String, usize>>>,
    child: Child,
    protocol: JoinHandle<async_lsp::Result<()>>,
    input: JoinHandle<std::io::Result<()>>,
    stderr: JoinHandle<std::io::Result<String>>,
}

impl Client {
    pub fn spawn(root: &Path, program: &str, args: &[String], config: Value) -> Result<Self> {
        let notifications = Arc::new(Mutex::new(BTreeMap::new()));
        let seen = notifications.clone();
        let (mainloop, server) = MainLoop::new_client(move |_| {
            let mut router = Router::<_, ResponseError>::new(config);
            router.request::<request::WorkspaceConfiguration, _>(|config, params| {
                let values = params
                    .items
                    .iter()
                    .map(|item| {
                        let section = item
                            .section
                            .as_deref()
                            .unwrap_or("")
                            .trim_start_matches("rust-analyzer.");
                        if section == "rust-analyzer" || section.is_empty() {
                            return config.clone();
                        }
                        section
                            .split('.')
                            .fold(&*config, |value, key| &value[key])
                            .clone()
                    })
                    .collect();
                async move { Ok(values) }
            });
            router.request::<request::WorkDoneProgressCreate, _>(|_, _| async { Ok(()) });
            // The spike advertises no dynamic registration; keep the callback responsive.
            router.request::<request::RegisterCapability, _>(|_, _| async { Ok(()) });
            router.request::<request::ApplyWorkspaceEdit, _>(|_, _| async {
                Ok(lsp_types::ApplyWorkspaceEditResponse {
                    applied: false,
                    failure_reason: Some("Read-only spike".into()),
                    failed_change: None,
                })
            });
            router.unhandled_notification(move |_, notification| {
                let mut counts = seen.lock().unwrap();
                if counts.contains_key(&notification.method) || counts.len() < 64 {
                    *counts.entry(notification.method).or_default() += 1;
                }
                ControlFlow::Continue(())
            });
            router
        });
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().ok_or("Missing stdout")?;
        let stdin = child.stdin.take().ok_or("Missing stdin")?;
        let mut errors = child.stderr.take().ok_or("Missing stderr")?;
        let (input_reader, input_writer) = tokio::io::duplex(64 * 1024);
        let input = tokio::spawn(forward_bounded(stdout, input_writer));
        let protocol =
            tokio::spawn(mainloop.run_buffered(input_reader.compat(), stdin.compat_write()));
        let stderr = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut bytes = [0; 4096];
            loop {
                let count = errors.read(&mut bytes).await?;
                if count == 0 {
                    break;
                }
                tail.extend_from_slice(&bytes[..count]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
            }
            Ok(String::from_utf8_lossy(&tail).into_owned())
        });
        Ok(Self {
            server,
            notifications,
            child,
            protocol,
            input,
            stderr,
        })
    }

    pub async fn request<R: request::Request>(
        &self,
        params: Value,
        deadline: Duration,
    ) -> Result<Value>
    where
        R::Result: serde::Serialize,
    {
        // shortcut: timeout does not cancel the peer, add request-ID cancellation before production use.
        let response = tokio::time::timeout(
            deadline,
            self.server.request::<R>(serde_json::from_value(params)?),
        )
        .await??;
        Ok(serde_json::to_value(response)?)
    }

    pub async fn initialize(&self, root: &Path, config: Value) -> Result<Value> {
        let uri = lsp_types::Url::from_directory_path(root).map_err(|_| "Invalid root")?;
        let result = self.request::<request::Initialize>(json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": "spike"}],
            "capabilities": {"general": {"positionEncodings": ["utf-16"]},
                "textDocument": {"callHierarchy": {"dynamicRegistration": false},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true}, "definition": {"linkSupport": true}},
                "window": {"workDoneProgress": true},
                "workspace": {"configuration": true}},
            "initializationOptions": config,
            "clientInfo": {"name": "oaklet-lsp-spike", "version": "1"}
        }), Duration::from_secs(60)).await?;
        self.server
            .notify::<notification::Initialized>(lsp_types::InitializedParams {})?;
        Ok(result)
    }

    pub async fn finish(&mut self) -> Result<Value> {
        let shutdown = self
            .request::<request::Shutdown>(Value::Null, Duration::from_secs(3))
            .await;
        let _ = self.server.notify::<notification::Exit>(());
        let status = match tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await {
            Ok(result) => result?,
            Err(_) => {
                self.child.kill().await?;
                self.child.wait().await?
            }
        };
        self.protocol.abort();
        let input_error = match tokio::time::timeout(Duration::from_secs(1), &mut self.input).await
        {
            Ok(Ok(Err(error))) => Some(error.to_string()),
            _ => None,
        };
        let stderr = match tokio::time::timeout(Duration::from_secs(1), &mut self.stderr).await {
            Ok(Ok(Ok(text))) => text,
            _ => "stderr drain did not finish".into(),
        };
        Ok(
            json!({"shutdown": shutdown.as_ref().map(|_| "ok").unwrap_or("failed"),
            "exitCode": status.code(), "inputError": input_error, "stderrTail": stderr,
            "notifications": *self.notifications.lock().unwrap()}),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.protocol.abort();
        self.input.abort();
        self.stderr.abort();
    }
}
