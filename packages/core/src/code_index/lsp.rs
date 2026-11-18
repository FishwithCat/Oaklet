use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_lsp::{MainLoop, ResponseError, ServerSocket, router::Router};
use lsp_types::{notification, request};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    Call, IndexError, Language, Location, Query, QueryData, QueryResult, ServerCapabilities,
    ServerCommand, Symbol,
};
use crate::tools::Cancel;

type Result<T> = std::result::Result<T, IndexError>;
fn backend(error: impl std::fmt::Display) -> IndexError {
    IndexError::Backend(error.to_string())
}

#[derive(Default)]
pub(super) struct ClientState {
    pub progress: Mutex<BTreeMap<String, String>>,
    pub watches: Mutex<super::watch::Registrations>,
}

pub(super) struct LanguageServerSession {
    socket: ServerSocket,
    child: Child,
    #[cfg(unix)]
    group: crate::tools::ProcessGroupGuard,
    tasks: Vec<JoinHandle<()>>,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<u8>>>,
    capabilities: Value,
    workspace: PathBuf,
    language: Language,
    documents: BTreeMap<PathBuf, (i32, String)>,
    client: Arc<ClientState>,
    pub generation: u64,
}

impl LanguageServerSession {
    pub async fn start(
        root: &Path,
        workspace: &Path,
        language: Language,
        command: &ServerCommand,
        generation: u64,
        cancel: &Cancel,
        client: Arc<ClientState>,
    ) -> Result<Self> {
        let config = match language {
            Language::Rust => {
                json!({"cargo": {"buildScripts": {"enable": false}}, "procMacro": {"enable": false}})
            }
            Language::TypeScript => json!({}),
        };
        let settings = config.clone();
        let watches = client.clone();
        let progress = client.clone();
        let watch_root = root.to_path_buf();
        let (mainloop, socket) = MainLoop::new_client(move |_| {
            let mut router = Router::<_, ResponseError>::new(settings);
            router.request::<request::WorkspaceConfiguration, _>(|config, params| {
                let values = params
                    .items
                    .iter()
                    .map(|item| {
                        let section = item.section.as_deref().unwrap_or("");
                        let section = section.strip_prefix("rust-analyzer.").unwrap_or(section);
                        if section.is_empty() || section == "rust-analyzer" {
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
            router.request::<request::ApplyWorkspaceEdit, _>(|_, _| async {
                Ok(lsp_types::ApplyWorkspaceEditResponse {
                    applied: false,
                    failure_reason: Some("Oaklet code index is read-only".into()),
                    failed_change: None,
                })
            });
            let registration = watches.clone();
            router.request::<request::RegisterCapability, _>(move |_, params| {
                let result = registration
                    .watches
                    .lock()
                    .unwrap()
                    .register(serde_json::to_value(params).unwrap(), &watch_root)
                    .map_err(|error| {
                        ResponseError::new(async_lsp::ErrorCode::INVALID_PARAMS, error.to_string())
                    });
                async move { result }
            });
            router.request::<request::UnregisterCapability, _>(move |_, params| {
                watches
                    .watches
                    .lock()
                    .unwrap()
                    .unregister(serde_json::to_value(params).unwrap());
                async { Ok(()) }
            });
            // Progress does not prove semantic readiness.
            router.unhandled_notification(move |_, notification| {
                if notification.method == "$/progress" {
                    let token = notification.params["token"].to_string();
                    let value = &notification.params["value"];
                    let mut active = progress.progress.lock().unwrap();
                    if value["kind"] == "end" {
                        active.remove(&token);
                    } else if token.len() <= 128
                        && (active.len() < 64 || active.contains_key(&token))
                    {
                        let message = value["message"]
                            .as_str()
                            .or_else(|| value["title"].as_str())
                            .unwrap_or("Analyzing");
                        active.insert(token, message.chars().take(1024).collect());
                    }
                }
                ControlFlow::Continue(())
            });
            router
        });
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .current_dir(root)
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process.spawn().map_err(backend)?;
        #[cfg(unix)]
        let group = crate::tools::ProcessGroupGuard::new(child.id());
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| backend("Missing stdout"))?;
        let stdin = child.stdin.take().ok_or_else(|| backend("Missing stdin"))?;
        let mut errors = child
            .stderr
            .take()
            .ok_or_else(|| backend("Missing stderr"))?;
        let (reader, writer) = tokio::io::duplex(64 * 1024);
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let tail = stderr.clone();
        let loop_socket = socket.clone();
        let tasks = vec![
            tokio::spawn(async move {
                let _ = super::framing::forward_bounded(stdout, writer).await;
            }),
            tokio::spawn(async move {
                // Keep the internal channel alive until the aborted protocol task actually exits.
                let _socket = loop_socket;
                let _ = mainloop
                    .run_buffered(reader.compat(), stdin.compat_write())
                    .await;
                running.store(false, Ordering::Release);
            }),
            tokio::spawn(async move {
                let mut buffer = [0; 4096];
                while let Ok(count) = errors.read(&mut buffer).await {
                    if count == 0 {
                        break;
                    }
                    let mut tail = tail.lock().unwrap();
                    tail.extend_from_slice(&buffer[..count]);
                    if tail.len() > 8192 {
                        let excess = tail.len() - 8192;
                        tail.drain(..excess);
                    }
                }
            }),
        ];
        let mut session = Self {
            socket,
            child,
            #[cfg(unix)]
            group,
            tasks,
            alive,
            stderr,
            capabilities: Value::Null,
            workspace: workspace.to_path_buf(),
            language,
            documents: BTreeMap::new(),
            client,
            generation,
        };
        let uri = lsp_types::Url::from_directory_path(root)
            .map_err(|_| backend("Invalid project URI"))?;
        let options = if language == Language::TypeScript {
            json!({"tsserver": {"path": command.typescript_sdk.as_ref().map(|path| path.join("tsserver.js"))}})
        } else {
            config.clone()
        };
        let initialized = session.request::<request::Initialize>(json!({
            "processId": std::process::id(), "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": root.file_name().unwrap_or_default().to_string_lossy()}],
            "capabilities": {"general": {"positionEncodings": ["utf-16", "utf-8", "utf-32"]},
                "textDocument": {"callHierarchy": {"dynamicRegistration": false},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true}, "definition": {"linkSupport": true}},
                "workspace": {"configuration": true, "didChangeWatchedFiles": {"dynamicRegistration": true, "relativePatternSupport": true}}, "window": {"workDoneProgress": true}},
            "initializationOptions": options, "clientInfo": {"name": "oaklet", "version": env!("CARGO_PKG_VERSION")}
        }), Duration::from_secs(60), cancel).await;
        match initialized {
            Ok(result) => {
                session.capabilities = result["capabilities"].clone();
                if session
                    .capabilities
                    .get("positionEncoding")
                    .and_then(Value::as_str)
                    .is_some_and(|encoding| !["utf-16", "utf-8", "utf-32"].contains(&encoding))
                {
                    session.finish().await;
                    return Err(IndexError::Unsupported(
                        "Server selected an unsupported position encoding".into(),
                    ));
                }
                session
                    .socket
                    .notify::<notification::Initialized>(lsp_types::InitializedParams {})
                    .map_err(backend)?;
                session
                    .socket
                    .notify::<notification::DidChangeConfiguration>(
                        lsp_types::DidChangeConfigurationParams {
                            settings: json!({"rust-analyzer": config}),
                        },
                    )
                    .map_err(backend)?;
                Ok(session)
            }
            Err(error) => {
                session.finish().await;
                Err(error)
            }
        }
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        let supported = |name: &str| {
            self.capabilities
                .get(name)
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
        };
        ServerCapabilities {
            symbols: supported("documentSymbolProvider"),
            definition: supported("definitionProvider"),
            references: supported("referencesProvider"),
            calls: supported("callHierarchyProvider"),
        }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn failure(&self) -> IndexError {
        backend(format!(
            "Language server stopped: {}",
            String::from_utf8_lossy(&self.stderr.lock().unwrap())
        ))
    }

    async fn request<R: request::Request>(
        &self,
        params: Value,
        deadline: Duration,
        cancel: &Cancel,
    ) -> Result<Value>
    where
        R::Result: serde::Serialize,
    {
        let params = serde_json::from_value(params).map_err(backend)?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(IndexError::Cancelled),
            result = tokio::time::timeout(deadline, self.socket.request::<R>(params)) => {
                match result {
                    Err(_) => Err(IndexError::Timeout),
                    Ok(Err(async_lsp::Error::Response(error))) if error.code == async_lsp::ErrorCode::METHOD_NOT_FOUND => Err(IndexError::Unsupported(R::METHOD.into())),
                    Ok(Err(async_lsp::Error::Response(error))) if error.code == async_lsp::ErrorCode::SERVER_NOT_INITIALIZED || error.code == async_lsp::ErrorCode::CONTENT_MODIFIED => Err(IndexError::NotReady),
                    Ok(Err(error)) => Err(backend(error)),
                    Ok(Ok(value)) => serde_json::to_value(value).map_err(backend),
                }
            }
        }
    }

    pub async fn background_symbols(
        &mut self,
        path: &Path,
        cancel: &Cancel,
    ) -> Result<(QueryResult, String)> {
        let already_open = self.documents.contains_key(path);
        let query = Query::Symbols {
            file: path.to_string_lossy().into_owned(),
        };
        let result =
            match tokio::time::timeout(Duration::from_secs(3), self.query(path, &query, cancel))
                .await
            {
                Ok(result) => result,
                Err(_) => Err(IndexError::Timeout),
            };
        let result = result.and_then(|result| {
            let hash = result.content_hash.clone().ok_or(IndexError::NotReady)?;
            Ok((result, hash))
        });
        if !already_open {
            self.close_document(path)?;
        }
        result
    }

    pub async fn query(
        &mut self,
        path: &Path,
        query: &Query,
        cancel: &Cancel,
    ) -> Result<QueryResult> {
        if !self.alive() {
            return Err(self.failure());
        }
        let capability = match query {
            Query::Symbols { .. } => "documentSymbolProvider",
            Query::Definition { .. } => "definitionProvider",
            Query::References { .. } => "referencesProvider",
            Query::Calls { .. } => "callHierarchyProvider",
        };
        if !self
            .capabilities
            .get(capability)
            .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
        {
            return Err(IndexError::Unsupported(capability.into()));
        }
        self.sync_document(path).await?;
        let uri = lsp_types::Url::from_file_path(path).map_err(|_| backend("Invalid file URI"))?;
        let position = match query {
            Query::Symbols { .. } => Value::Null,
            Query::Definition { line, column, .. }
            | Query::References { line, column, .. }
            | Query::Calls { line, column, .. } => {
                let position = super::documents::convert(
                    &self.documents[path].1,
                    lsp_types::Position::new(*line, *column),
                    "utf-16",
                    self.encoding(),
                )?;
                json!(position)
            }
        };
        let params = json!({"textDocument": {"uri": uri}, "position": position});
        let deadline = Duration::from_secs(30);
        let mut data = match query {
            Query::Symbols { .. } => {
                let value = self
                    .request::<request::DocumentSymbolRequest>(params, deadline, cancel)
                    .await?;
                QueryData::Symbols(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|item| symbol(item, &uri))
                        .collect::<Result<_>>()?,
                )
            }
            Query::Definition { .. } => {
                let value = self
                    .request::<request::GotoDefinition>(params, deadline, cancel)
                    .await?;
                let items: Vec<_> = if value.is_null() {
                    vec![]
                } else if let Some(items) = value.as_array() {
                    items.clone()
                } else {
                    vec![value]
                };
                QueryData::Locations(
                    items
                        .iter()
                        .map(|item| {
                            if item.get("targetUri").is_some() {
                                location(&item["targetUri"], &item["targetSelectionRange"])
                            } else {
                                location(&item["uri"], &item["range"])
                            }
                        })
                        .collect::<Result<_>>()?,
                )
            }
            Query::References { .. } => {
                let mut params = params;
                params["context"] = json!({"includeDeclaration": true});
                let value = self
                    .request::<request::References>(params, deadline, cancel)
                    .await?;
                QueryData::Locations(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|item| location(&item["uri"], &item["range"]))
                        .collect::<Result<_>>()?,
                )
            }
            Query::Calls { incoming, .. } => {
                let items = self
                    .request::<request::CallHierarchyPrepare>(params, deadline, cancel)
                    .await?;
                let mut calls = Vec::new();
                for item in items.as_array().into_iter().flatten().take(32) {
                    let params = json!({"item": item});
                    let result = if *incoming {
                        self.request::<request::CallHierarchyIncomingCalls>(
                            params, deadline, cancel,
                        )
                        .await?
                    } else {
                        self.request::<request::CallHierarchyOutgoingCalls>(
                            params, deadline, cancel,
                        )
                        .await?
                    };
                    for call in result.as_array().into_iter().flatten() {
                        let target = &call[if *incoming { "from" } else { "to" }];
                        let site_uri = if *incoming {
                            &target["uri"]
                        } else {
                            &item["uri"]
                        };
                        calls.push(Call {
                            name: target["name"].as_str().unwrap_or_default().into(),
                            target: location(&target["uri"], &target["selectionRange"])?,
                            sites: call["fromRanges"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|range| location(site_uri, range))
                                .collect::<Result<_>>()?,
                        });
                    }
                }
                let mut data = QueryData::Calls(calls);
                super::documents::normalize(&mut data, &self.workspace, self.encoding()).await?;
                return Ok(QueryResult {
                    content_hash: None,
                    source: super::QuerySource::Live,
                    coverage: Default::default(),
                    data,
                    generation: self.generation,
                    truncated: items.as_array().is_some_and(|items| items.len() > 32),
                    completeness_known: false,
                });
            }
        };
        super::documents::normalize(&mut data, &self.workspace, self.encoding()).await?;
        Ok(QueryResult {
            content_hash: if matches!(query, Query::Symbols { .. }) {
                self.documents
                    .get(path)
                    .map(|(_, text)| blake3::hash(text.as_bytes()).to_hex().to_string())
            } else {
                None
            },
            source: super::QuerySource::Live,
            coverage: Default::default(),
            data,
            generation: self.generation,
            truncated: false,
            completeness_known: false,
        })
    }

    fn encoding(&self) -> &str {
        self.capabilities["positionEncoding"]
            .as_str()
            .unwrap_or("utf-16")
    }

    fn close_document(&mut self, path: &Path) -> Result<()> {
        if self.documents.remove(path).is_some()
            && (self.capabilities["textDocumentSync"]["openClose"] == true
                || self.capabilities["textDocumentSync"].is_number())
        {
            let uri =
                lsp_types::Url::from_file_path(path).map_err(|_| backend("Invalid file URI"))?;
            self.socket
                .notify::<notification::DidCloseTextDocument>(
                    lsp_types::DidCloseTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri },
                    },
                )
                .map_err(backend)?;
        }
        Ok(())
    }

    async fn sync_document(&mut self, path: &Path) -> Result<()> {
        let text = super::documents::read(&self.workspace, path).await?;
        let sync = &self.capabilities["textDocumentSync"];
        let mode = sync
            .as_u64()
            .or_else(|| sync["change"].as_u64())
            .unwrap_or(0);
        // The legacy numeric form implies open/close support.
        let open_close = sync.is_number() && mode != 0 || sync["openClose"] == true;
        let uri = lsp_types::Url::from_file_path(path).map_err(|_| backend("Invalid file URI"))?;
        // Bound retained overlays as well as each individual file.
        while self
            .documents
            .values()
            .map(|(_, text)| text.len())
            .sum::<usize>()
            - self.documents.get(path).map_or(0, |(_, old)| old.len())
            + text.len()
            > 32 * 1024 * 1024
        {
            let evicted = self
                .documents
                .keys()
                .find(|key| key.as_path() != path)
                .unwrap()
                .clone();
            self.close_document(&evicted)?;
        }
        let existing = self.documents.get(path);
        let changed = existing.is_some();
        if existing.is_some_and(|(_, old)| old == &text) {
            return Ok(());
        }
        if let Some((version, old)) = existing {
            if mode == 0 {
                return Err(IndexError::Unsupported(
                    "Server does not accept saved document changes".into(),
                ));
            }
            let version = version
                .checked_add(1)
                .ok_or_else(|| backend("Document version exhausted"))?;
            let range = (mode == 2).then(|| {
                lsp_types::Range::new(
                    lsp_types::Position::new(0, 0),
                    super::documents::end(old, self.encoding()),
                )
            });
            self.socket
                .notify::<notification::DidChangeTextDocument>(
                    lsp_types::DidChangeTextDocumentParams {
                        text_document: lsp_types::VersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version,
                        },
                        content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                            range,
                            range_length: None,
                            text: text.clone(),
                        }],
                    },
                )
                .map_err(backend)?;
            self.documents
                .insert(path.to_path_buf(), (version, text.clone()));
        } else {
            if self.documents.len() >= 64 {
                let evicted = self.documents.keys().next().unwrap().clone();
                self.close_document(&evicted)?;
            }
            let language_id = match path.extension().and_then(|ext| ext.to_str()) {
                Some("tsx") => "typescriptreact",
                Some("jsx") => "javascriptreact",
                Some("js" | "mjs" | "cjs") => "javascript",
                _ if self.language == Language::Rust => "rust",
                _ => "typescript",
            };
            if open_close {
                self.socket
                    .notify::<notification::DidOpenTextDocument>(
                        lsp_types::DidOpenTextDocumentParams {
                            text_document: lsp_types::TextDocumentItem {
                                uri: uri.clone(),
                                language_id: language_id.into(),
                                version: 1,
                                text: text.clone(),
                            },
                        },
                    )
                    .map_err(backend)?;
            }
            self.documents.insert(path.to_path_buf(), (1, text.clone()));
        }
        let save = &self.capabilities["textDocumentSync"]["save"];
        if changed && (save == &Value::Bool(true) || save.is_object()) {
            self.socket
                .notify::<notification::DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri },
                    text: (save["includeText"] == true).then_some(text),
                })
                .map_err(backend)?;
        }
        Ok(())
    }

    pub async fn files_changed(&mut self, changes: &[(PathBuf, u64)]) -> Result<()> {
        let mut events = Vec::new();
        for (path, kind) in changes {
            if self.documents.contains_key(path) {
                if *kind == 3 {
                    self.close_document(path)?;
                } else {
                    self.sync_document(path).await?;
                }
            }
            if self.client.watches.lock().unwrap().matches(path, *kind) {
                events.push(lsp_types::FileEvent {
                    uri: lsp_types::Url::from_file_path(path)
                        .map_err(|_| backend("Invalid file URI"))?,
                    typ: match kind {
                        1 => lsp_types::FileChangeType::CREATED,
                        2 => lsp_types::FileChangeType::CHANGED,
                        _ => lsp_types::FileChangeType::DELETED,
                    },
                });
            }
        }
        if !events.is_empty() {
            self.socket
                .notify::<notification::DidChangeWatchedFiles>(
                    lsp_types::DidChangeWatchedFilesParams { changes: events },
                )
                .map_err(backend)?;
        }
        Ok(())
    }

    pub async fn finish(&mut self) {
        if self.alive() {
            let (_owner, cancel) = crate::tools::CancelHandle::new();
            let _ = self
                .request::<request::Shutdown>(Value::Null, Duration::from_secs(2), &cancel)
                .await;
            let _ = self.socket.notify::<notification::Exit>(());
        }
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        #[cfg(unix)]
        {
            self.group.kill();
            self.group.disarm();
        }
        self.alive.store(false, Ordering::Release);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for LanguageServerSession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn location(uri: &Value, range: &Value) -> Result<Location> {
    let uri: lsp_types::Url = serde_json::from_value(uri.clone()).map_err(backend)?;
    let range: lsp_types::Range = serde_json::from_value(range.clone()).map_err(backend)?;
    Ok(Location {
        path: uri
            .to_file_path()
            .map_err(|_| IndexError::Unsupported("Non-file location".into()))?,
        line: range.start.line,
        column: range.start.character,
        end_line: range.end.line,
        end_column: range.end.character,
    })
}
fn symbol(item: &Value, uri: &lsp_types::Url) -> Result<Symbol> {
    let location = if item.get("location").is_some() {
        location(&item["location"]["uri"], &item["location"]["range"])?
    } else {
        location(&json!(uri), &item["selectionRange"])?
    };
    Ok(Symbol {
        name: item["name"]
            .as_str()
            .ok_or_else(|| backend("Missing symbol name"))?
            .into(),
        kind: item["kind"]
            .as_u64()
            .ok_or_else(|| backend("Missing symbol kind"))? as u32,
        location,
        children: item["children"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| symbol(item, uri))
            .collect::<Result<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{CancelHandle, test_support::temp_root};

    #[tokio::test]
    async fn request_timeout_sends_cancel_and_preserves_the_session() {
        let root = temp_root("index-request-timeout");
        let program = super::super::runtime::find_program(
            "python3",
            &root,
            std::env::var_os("PATH").as_deref(),
        )
        .unwrap();
        let command = ServerCommand {
            program,
            args: vec![
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/code_index/fake_server.py")
                    .to_string_lossy()
                    .into_owned(),
            ],
            version: "fake".into(),
            runtime_versions: vec![],
            typescript_sdk: None,
        };
        let (_owner, cancel) = CancelHandle::new();
        let mut session = LanguageServerSession::start(
            &root,
            &root,
            Language::Rust,
            &command,
            1,
            &cancel,
            Arc::new(ClientState::default()),
        )
        .await
        .unwrap();
        std::fs::write(root.join("lib.rs"), "pub fn alive() {}\n").unwrap();
        let params = json!({"textDocument": {"uri": lsp_types::Url::from_file_path(root.join("lib.rs")).unwrap()}, "position": {"line": 0, "character": 0}});
        assert_eq!(
            session
                .request::<request::GotoDefinition>(params, Duration::from_millis(30), &cancel)
                .await
                .unwrap_err(),
            IndexError::Timeout
        );
        let result = session
            .query(
                &root.join("lib.rs"),
                &Query::Symbols {
                    file: "lib.rs".into(),
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(matches!(result.data, QueryData::Symbols(items) if items[0].name == "alive"));
        std::fs::write(root.join("method-missing"), "").unwrap();
        assert!(matches!(
            session
                .query(
                    &root.join("lib.rs"),
                    &Query::Symbols {
                        file: "lib.rs".into()
                    },
                    &cancel
                )
                .await,
            Err(IndexError::Unsupported(_))
        ));
        session.finish().await;
        let tasks = std::mem::take(&mut session.tasks);
        drop(session);
        for task in tasks {
            if let Err(error) = task.await {
                assert!(error.is_cancelled(), "protocol task panicked: {error}");
            }
        }
        let events = std::fs::read_to_string(root.join("events.jsonl")).unwrap();
        assert!(events.contains("\"event\": \"cancel\""));
        std::fs::remove_dir_all(root).unwrap();
    }
}
