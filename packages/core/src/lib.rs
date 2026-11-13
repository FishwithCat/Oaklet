mod agent;
mod architecture;
mod chat;
mod compaction;
mod connection;
mod context;
mod files;
mod git;
mod repeat;
mod spill;
mod store;
mod terminal;
mod tools;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub config_path: PathBuf,
    pub approvals: Mutex<HashMap<String, PendingApproval>>,
    pub chat: chat::ChatState,
    /// Canonical workspace where `run_command` approvals were waived for this
    /// session. `None` means every command is confirmed.
    pub command_allow_all: Mutex<Option<PathBuf>>,
    /// Shells behind the terminal view, keyed by the frontend's session id.
    pub terminal: Mutex<HashMap<String, terminal::Session>>,
    /// Background commands started through `run_command` with
    /// `run_in_background`; shared across runs and stopped on app exit.
    pub jobs: Arc<tools::jobs::JobRegistry>,
    /// Last valid architecture model and the in-flight compiler generation.
    pub architecture: Mutex<architecture::ArchitectureCache>,
    ids: AtomicU64,
}

pub struct PendingApproval {
    pub run_id: String,
    /// Workspace the command will run in, so a grant stays scoped to it.
    pub workspace: PathBuf,
    pub sender: oneshot::Sender<bool>,
}

impl AppState {
    pub fn next_id(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = self.ids.fetch_add(1, Ordering::Relaxed);
        format!("{millis:x}-{seq:x}")
    }
}

/// Only shell commands in the exact workspace the user granted may skip the
/// approval prompt; every other request still asks.
fn auto_approves_command(tool: &str, workspace: &Path, granted: Option<&Path>) -> bool {
    tool == "run_command" && granted == Some(workspace)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Model for compaction summaries. Empty reuses `model` so the summary
    /// request can hit the same provider prefix cache.
    #[serde(default)]
    pub summary_model: String,
    /// Prompt tokens the model accepts. History is compacted near this size.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_search_url")]
    pub search_url: String,
    /// Key for the search endpoint. Empty reuses `api_key`.
    #[serde(default)]
    pub search_api_key: String,
    /// Skip the approval card for `run_command` in the configured workspace.
    /// On by default; the sidebar leaf still revokes it for this session.
    #[serde(default = "default_true")]
    pub allow_all_commands: bool,
    #[serde(default, deserialize_with = "deserialize_mcp_servers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    #[serde(default = "default_workspace")]
    pub workspace: String,
}

/// How Oaklet reaches an MCP server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpTransport {
    #[default]
    Stdio,
    #[serde(alias = "http", alias = "streamable_http")]
    StreamableHttp,
}

/// One user-configured MCP server. Only the fields for its transport apply.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<McpTransport>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl McpServerConfig {
    /// Explicit type when present; command-first when omitted, matching common
    /// `mcpServers` configs.
    pub fn transport(&self) -> McpTransport {
        self.kind.unwrap_or_else(|| {
            if !self.command.trim().is_empty() {
                McpTransport::Stdio
            } else if !self.url.trim().is_empty() {
                McpTransport::StreamableHttp
            } else {
                McpTransport::Stdio
            }
        })
    }
}

/// Accept both the standard `mcpServers` object and the older array shape.
fn deserialize_mcp_servers<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, McpServerConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Servers {
        Map(BTreeMap<String, McpServerConfig>),
        List(Vec<LegacyServer>),
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyServer {
        #[serde(default)]
        name: String,
        #[serde(flatten)]
        config: McpServerConfig,
    }

    let mut servers = BTreeMap::new();
    match Servers::deserialize(deserializer)? {
        Servers::Map(map) => servers = map,
        Servers::List(list) => {
            for server in list {
                if !server.name.trim().is_empty() {
                    servers.insert(server.name, server.config);
                }
            }
        }
    }
    Ok(servers)
}

fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}

fn default_model() -> String {
    "deepseek-flash".into()
}

fn default_context_window() -> usize {
    262_144
}

fn default_search_url() -> String {
    tools::web::DEFAULT_ENDPOINT.into()
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_workspace() -> String {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            api_key: String::new(),
            model: default_model(),
            summary_model: String::new(),
            context_window: default_context_window(),
            search_url: default_search_url(),
            search_api_key: String::new(),
            allow_all_commands: true,
            mcp_servers: BTreeMap::new(),
            workspace: default_workspace(),
        }
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            kind: None,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            enabled: true,
        }
    }
}

/// Trim user input, drop blank server entries, and reject incomplete enabled
/// servers before they reach the agent loop.
fn normalize_mcp_servers(
    servers: BTreeMap<String, McpServerConfig>,
) -> Result<BTreeMap<String, McpServerConfig>, String> {
    let mut normalized = BTreeMap::new();

    for (name, mut server) in servers {
        let name = name.trim().to_string();
        server.command = server.command.trim().to_string();
        server.url = server.url.trim().to_string();
        server.env = clean_pairs(server.env)?;
        server.headers = clean_pairs(server.headers)?;

        if server.command.is_empty()
            && server.url.is_empty()
            && server.args.is_empty()
            && server.env.is_empty()
            && server.headers.is_empty()
        {
            continue;
        }
        if name.is_empty() {
            return Err("Every MCP server needs a name.".into());
        }
        if normalized.contains_key(&name) {
            return Err(format!("Duplicate MCP server name: {name}."));
        }
        let transport = server.transport();
        server.kind = Some(transport);
        if !server.enabled {
            normalized.insert(name, server);
            continue;
        }

        match transport {
            McpTransport::Stdio => {
                if server.command.is_empty() {
                    return Err(format!("MCP server \"{name}\" needs a command."));
                }
            }
            McpTransport::StreamableHttp => {
                let parsed = reqwest::Url::parse(&server.url)
                    .map_err(|_| format!("MCP server \"{name}\" needs a valid URL."))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(format!(
                        "MCP server \"{name}\" needs an http:// or https:// URL."
                    ));
                }
                validate_headers(&name, &server)?;
            }
        }
        normalized.insert(name, server);
    }

    Ok(normalized)
}

fn clean_pairs(pairs: BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    let mut cleaned = BTreeMap::new();
    for (key, value) in pairs {
        let key = key.trim().to_string();
        let value = value.trim().to_string();
        if key.is_empty() {
            continue;
        }
        if key.contains('=') || key.contains('\0') {
            return Err(format!("Invalid name: {key}"));
        }
        cleaned.insert(key, value);
    }
    Ok(cleaned)
}

fn validate_headers(server_name: &str, server: &McpServerConfig) -> Result<(), String> {
    for (header_name, value) in &server.headers {
        reqwest::header::HeaderName::from_bytes(header_name.as_bytes()).map_err(|_| {
            format!("MCP server \"{server_name}\" has an invalid header name: {header_name}.")
        })?;
        reqwest::header::HeaderValue::from_str(value).map_err(|_| {
            format!("MCP server \"{server_name}\" has an invalid header value for {header_name}.")
        })?;
    }
    Ok(())
}

fn read_config(path: &Path) -> Option<AppConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create config dir: {e}"))?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Could not encode config: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("Could not write config: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[tauri::command]
fn load_config(state: State<'_, SharedState>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn save_config(state: State<'_, SharedState>, mut config: AppConfig) -> Result<AppConfig, String> {
    config.base_url = config.base_url.trim().trim_end_matches('/').to_string();
    config.api_key = config.api_key.trim().to_string();
    config.model = config.model.trim().to_string();
    config.summary_model = config.summary_model.trim().to_string();
    config.search_api_key = config.search_api_key.trim().to_string();
    config.search_url = config.search_url.trim().trim_end_matches('/').to_string();
    config.workspace = config.workspace.trim().to_string();

    if config.search_url.is_empty() {
        config.search_url = default_search_url();
    }
    if !(config.base_url.starts_with("http://") || config.base_url.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://.".into());
    }
    if !(config.search_url.starts_with("http://") || config.search_url.starts_with("https://")) {
        return Err("Search URL must start with http:// or https://.".into());
    }
    if config.model.is_empty() {
        return Err("Model is required.".into());
    }
    if config.context_window < 8_192 {
        return Err("Context window must be at least 8192 tokens.".into());
    }
    config.mcp_servers = normalize_mcp_servers(config.mcp_servers)?;
    if !Path::new(&config.workspace).is_dir() {
        return Err(format!(
            "Workspace is not a directory: {}",
            config.workspace
        ));
    }
    if let Ok(canonical) = Path::new(&config.workspace).canonicalize() {
        config.workspace = canonical.to_string_lossy().into_owned();
    }

    let previous = state.config.lock().unwrap().clone();
    write_config(&state.config_path, &config)?;
    *state.config.lock().unwrap() = config.clone();
    if (previous.workspace != config.workspace
        || previous.allow_all_commands != config.allow_all_commands)
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        *granted = config
            .allow_all_commands
            .then(|| PathBuf::from(&config.workspace));
    }
    if previous.workspace != config.workspace {
        chat::stop_all(state.inner());
        architecture::cancel_for_workspace_change(state.inner());
    }
    Ok(config)
}

#[tauri::command]
async fn architecture_discover(
    app: AppHandle,
    state: State<'_, SharedState>,
    include_ignored: bool,
) -> Result<architecture::Discovery, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        architecture::discover(&app, &workspace, include_ignored)
    })
    .await
    .map_err(|error| format!("Could not scan the workspace: {error}"))?
}

#[tauri::command]
async fn architecture_load(
    app: AppHandle,
    state: State<'_, SharedState>,
    root: String,
    include_ignored: bool,
) -> Result<architecture::LoadResult, String> {
    let workspace = workspace_path(&state)?;
    let shared = state.inner().clone();
    architecture::load(app, shared, workspace, root, include_ignored).await
}

#[tauri::command]
fn approve_tool(
    state: State<'_, SharedState>,
    request_id: String,
    allow: bool,
    always: bool,
) -> Result<(), String> {
    let pending = state
        .approvals
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .remove(&request_id);
    let Some(pending) = pending else {
        return Err("This approval request has expired.".into());
    };

    pending
        .sender
        .send(allow)
        .map_err(|_| "The agent is no longer waiting for this action.".to_string())?;

    // A grant is session-only and scoped to the workspace the approved command
    // runs in, not to whatever workspace settings show later.
    if allow
        && always
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        *granted = Some(pending.workspace);
    }
    Ok(())
}

#[tauri::command]
fn command_allow_all_active(state: State<'_, SharedState>) -> bool {
    state
        .command_allow_all
        .lock()
        .map(|granted| granted.is_some())
        .unwrap_or(false)
}

#[tauri::command]
fn revoke_command_allow_all(state: State<'_, SharedState>) {
    if let Ok(mut granted) = state.command_allow_all.lock() {
        *granted = None;
    }
}

/// Read-only snapshot of discovered skills for Settings.
#[tauri::command]
async fn skills_list(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<tools::skill::SkillList, String> {
    let workspace = {
        let config = state
            .config
            .lock()
            .map_err(|_| "Agent state is unavailable.".to_string())?;
        PathBuf::from(&config.workspace)
    };
    let home = app.path().home_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        tools::skill::SkillCatalog::discover(&workspace, home.as_deref()).into_list()
    })
    .await
    .map_err(|error| format!("Could not scan skills: {error}"))
}

fn workspace_path(state: &AppState) -> Result<PathBuf, String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?;
    let workspace = PathBuf::from(&config.workspace);
    if !workspace.is_dir() {
        return Err("Choose an existing workspace folder first.".into());
    }
    Ok(workspace)
}

#[tauri::command]
async fn git_log(
    state: State<'_, SharedState>,
    include_commits: bool,
) -> Result<git::GitLog, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::log(&workspace, include_commits))
        .await
        .map_err(|error| format!("Could not read git history: {error}"))?
}

#[tauri::command]
async fn git_show(state: State<'_, SharedState>, hash: String) -> Result<String, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::show(&workspace, &hash))
        .await
        .map_err(|error| format!("Could not read the commit: {error}"))?
}

#[tauri::command]
async fn git_switch(state: State<'_, SharedState>, branch: String) -> Result<(), String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::switch(&workspace, &branch))
        .await
        .map_err(|error| format!("Could not switch branches: {error}"))?
}

#[tauri::command]
async fn git_changes(state: State<'_, SharedState>) -> Result<Vec<git::GitChange>, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::changes(&workspace))
        .await
        .map_err(|error| format!("Could not read uncommitted changes: {error}"))?
}

#[tauri::command]
async fn git_diff(state: State<'_, SharedState>, path: String) -> Result<String, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::diff(&workspace, &path))
        .await
        .map_err(|error| format!("Could not read the file diff: {error}"))?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    id: String,
    sequence: u64,
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalExit {
    id: String,
    code: Option<u32>,
}

#[tauri::command]
fn terminal_open(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let workspace = {
        let config = state
            .config
            .lock()
            .map_err(|_| "Agent state is unavailable.".to_string())?;
        PathBuf::from(&config.workspace)
    };
    if !workspace.is_dir() {
        return Err("Choose an existing workspace folder first.".into());
    }

    let mut sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    let (session, events) = terminal::spawn(&workspace, cols, rows)?;
    let output_flow = session.output_flow.clone();
    sessions.insert(id.clone(), session);
    drop(sessions);

    let shared = state.inner().clone();
    let session_id = id;
    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(event) = events.recv() {
            match event {
                terminal::Event::Output(data) => {
                    let Some(sequence) = output_flow.reserve(data.len()) else {
                        break;
                    };
                    if app
                        .emit(
                            "terminal://output",
                            TerminalOutput {
                                id: session_id.clone(),
                                sequence,
                                data,
                            },
                        )
                        .is_err()
                    {
                        break;
                    }
                }
                terminal::Event::Exit { code } => {
                    let _ = app.emit(
                        "terminal://exit",
                        TerminalExit {
                            id: session_id.clone(),
                            code,
                        },
                    );
                    break;
                }
            }
        }
        if let Ok(mut sessions) = shared.terminal.lock() {
            sessions.remove(&session_id);
        }
    });
    Ok(())
}

#[tauri::command]
fn terminal_ack(state: State<'_, SharedState>, id: String, sequence: u64) {
    if let Ok(sessions) = state.terminal.lock()
        && let Some(session) = sessions.get(&id)
    {
        session.output_flow.acknowledge(sequence);
    }
}

#[tauri::command]
fn terminal_write(state: State<'_, SharedState>, id: String, data: String) -> Result<(), String> {
    let mut sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    let Some(session) = sessions.get_mut(&id) else {
        return Ok(());
    };
    session.write(data.as_bytes())
}

#[tauri::command]
fn terminal_resize(
    state: State<'_, SharedState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state
        .terminal
        .lock()
        .map_err(|_| "Terminal state is unavailable.".to_string())?;
    if let Some(session) = sessions.get(&id) {
        session.resize(cols, rows)?;
    }
    Ok(())
}

#[tauri::command]
fn terminal_close(state: State<'_, SharedState>, id: String) {
    if let Ok(mut sessions) = state.terminal.lock() {
        sessions.remove(&id);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("config.json");
            let mut config = read_config(&config_path).unwrap_or_default();
            if config.workspace.trim().is_empty() || !Path::new(&config.workspace).is_dir() {
                config.workspace = default_workspace();
            }
            if let Ok(canonical) = Path::new(&config.workspace).canonicalize() {
                config.workspace = canonical.to_string_lossy().into_owned();
            }
            let data_dir = app.path().app_data_dir()?;
            let chat = chat::ChatState::open(&data_dir.join("oaklet.db"))
                .map_err(std::io::Error::other)?;
            let command_allow_all = config
                .allow_all_commands
                .then(|| PathBuf::from(&config.workspace));
            app.manage(Arc::new(AppState {
                config: Mutex::new(config),
                config_path,
                approvals: Mutex::new(HashMap::new()),
                chat,
                command_allow_all: Mutex::new(command_allow_all),
                terminal: Mutex::new(HashMap::new()),
                jobs: Arc::new(tools::jobs::JobRegistry::new()),
                architecture: Mutex::new(architecture::ArchitectureCache::default()),
                ids: AtomicU64::new(0),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            connection::test_connection,
            chat::session_create,
            chat::session_list,
            chat::session_get,
            chat::session_send,
            chat::session_stop,
            chat::session_compact,
            chat::session_import,
            approve_tool,
            command_allow_all_active,
            revoke_command_allow_all,
            skills_list,
            files::files_list,
            files::files_read,
            files::files_save,
            git_log,
            git_show,
            git_switch,
            git_changes,
            git_diff,
            architecture_discover,
            architecture_load,
            terminal_open,
            terminal_write,
            terminal_ack,
            terminal_resize,
            terminal_close
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
            ) {
                architecture::cancel_all(app.state::<SharedState>().inner());
                chat::stop_all(app.state::<SharedState>().inner());
                app.state::<SharedState>().jobs.kill_all();
            }
        });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{
        AppConfig, McpServerConfig, McpTransport, auto_approves_command, normalize_mcp_servers,
    };

    #[test]
    fn configs_saved_before_search_gets_the_deepseek_endpoint() {
        let config: AppConfig = serde_json::from_str(
            r#"{"baseUrl":"https://api.deepseek.com","apiKey":"k","model":"m","workspace":"/tmp"}"#,
        )
        .unwrap();
        assert_eq!(
            config.search_url,
            "https://api.deepseek.com/v1/tools/search"
        );
        assert!(config.summary_model.is_empty());
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn configs_saved_before_context_window_default_to_256k() {
        let config: AppConfig = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(config.context_window, 262_144);
    }

    #[test]
    fn configs_saved_before_allow_all_commands_default_to_allowed() {
        let config: AppConfig = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert!(config.allow_all_commands);
    }

    #[test]
    fn normalizes_mcp_servers_and_drops_blank_rows() {
        let servers: BTreeMap<String, McpServerConfig> = serde_json::from_str(
            r#"{
                " files ": {"command":" npx ","args":["server"]},
                "blank": {},
                "remote": {
                    "type":"streamableHttp",
                    "url":"https://example.com/mcp/",
                    "headers":{"Authorization":"Bearer token"}
                }
            }"#,
        )
        .unwrap();
        let servers = normalize_mcp_servers(servers).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers["files"].command, "npx");
        assert_eq!(servers["files"].kind, Some(McpTransport::Stdio));
        assert_eq!(servers["remote"].transport(), McpTransport::StreamableHttp);
        assert_eq!(servers["remote"].url, "https://example.com/mcp/");
    }

    #[test]
    fn rejects_incomplete_or_duplicate_mcp_servers() {
        let incomplete: BTreeMap<String, McpServerConfig> =
            serde_json::from_str(r#"{"files":{"args":["x"]}}"#).unwrap();
        assert!(
            normalize_mcp_servers(incomplete)
                .unwrap_err()
                .contains("command")
        );

        let duplicate: BTreeMap<String, McpServerConfig> = serde_json::from_str(
            r#"{
                " files ": {"command":"a"},
                "files": {"command":"b"}
            }"#,
        )
        .unwrap();
        assert!(
            normalize_mcp_servers(duplicate)
                .unwrap_err()
                .contains("Duplicate")
        );
    }

    #[test]
    fn disabled_servers_keep_incomplete_settings() {
        let servers: BTreeMap<String, McpServerConfig> =
            serde_json::from_str(r#"{"off":{"args":["x"],"enabled":false}}"#).unwrap();
        let servers = normalize_mcp_servers(servers).unwrap();
        assert_eq!(servers.len(), 1);
        assert!(!servers["off"].enabled);
    }

    #[test]
    fn mcp_servers_serialize_in_camel_case() {
        let config = AppConfig {
            mcp_servers: BTreeMap::from([(
                "remote".into(),
                McpServerConfig {
                    kind: Some(McpTransport::StreamableHttp),
                    url: "https://example.com/mcp".into(),
                    ..McpServerConfig::default()
                },
            )]),
            ..AppConfig::default()
        };
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["mcpServers"]["remote"]["type"], "streamableHttp");
        assert!(value["mcpServers"]["remote"].get("enabled").is_none());
    }

    #[test]
    fn infers_transport_and_reads_the_old_array_shape() {
        let inferred: BTreeMap<String, McpServerConfig> =
            serde_json::from_str(r#"{"remote":{"url":"https://example.com/mcp"}}"#).unwrap();
        assert_eq!(inferred["remote"].transport(), McpTransport::StreamableHttp);
        let alias: McpServerConfig =
            serde_json::from_str(r#"{"type":"http","url":"https://example.com"}"#).unwrap();
        assert_eq!(alias.transport(), McpTransport::StreamableHttp);

        let legacy: AppConfig = serde_json::from_str(
            r#"{"mcpServers":[{"name":"files","transport":"stdio","command":"npx"}]}"#,
        )
        .unwrap();
        assert_eq!(legacy.mcp_servers["files"].command, "npx");
        assert_eq!(legacy.mcp_servers["files"].transport(), McpTransport::Stdio);

        let legacy_http: AppConfig = serde_json::from_str(
            r#"{"mcpServers":[{"name":"remote","transport":"http","url":"https://example.com/mcp"}]}"#,
        )
        .unwrap();
        assert_eq!(
            legacy_http.mcp_servers["remote"].transport(),
            McpTransport::StreamableHttp
        );
    }

    #[test]
    fn auto_approves_only_commands_in_the_granted_workspace() {
        let workspace = Path::new("/tmp/oaklet-workspace");
        let other = Path::new("/tmp/oaklet-other");

        assert!(auto_approves_command(
            "run_command",
            workspace,
            Some(workspace)
        ));
        assert!(!auto_approves_command(
            "run_command",
            other,
            Some(workspace)
        ));
        assert!(!auto_approves_command("run_command", workspace, None));
        assert!(!auto_approves_command(
            "write_file",
            workspace,
            Some(workspace)
        ));
    }
}
