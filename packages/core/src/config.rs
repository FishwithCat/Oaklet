use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, WebviewWindow};

use crate::windows::workspace_for_window;
use crate::{AppState, SharedState, tools};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// UI theme: `light`, `dark`, or `auto` (follow the OS).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Model for compaction summaries. Empty reuses `model` so the summary
    /// request can hit the same provider prefix cache.
    #[serde(default)]
    pub summary_model: String,
    /// Required capacity when an independent summary model is selected.
    #[serde(default)]
    pub summary_context_window: Option<usize>,
    /// Prompt tokens the model accepts. History is compacted near this size.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_search_url")]
    pub search_url: String,
    /// Key for the search endpoint. Empty reuses `api_key`.
    #[serde(default)]
    pub search_api_key: String,
    /// Skip the approval card for `run_command` in the configured workspace.
    /// On by default; the sidebar toggle grants or revokes it for this session.
    #[serde(default = "default_true")]
    pub allow_all_commands: bool,
    #[serde(default)]
    pub keep_worktrees_on_archive: bool,
    #[serde(default, deserialize_with = "deserialize_mcp_servers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    /// Last opened workspace. Each window's active workspace lives in
    /// `WindowRegistry`; this value only chooses what the next launch opens.
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

fn default_theme() -> String {
    "auto".into()
}

fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn default_workspace() -> String {
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
            theme: default_theme(),
            summary_model: String::new(),
            summary_context_window: None,
            context_window: default_context_window(),
            search_url: default_search_url(),
            search_api_key: String::new(),
            allow_all_commands: true,
            keep_worktrees_on_archive: false,
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

pub(crate) fn read_config(path: &Path) -> Option<AppConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub(crate) fn write_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create config dir: {e}"))?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Could not encode config: {e}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_file_name(format!(".config-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("Could not create config temp file: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Could not protect config temp file: {e}"))?;
        }
        file.write_all(text.as_bytes())
            .map_err(|e| format!("Could not write config: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Could not flush config: {e}"))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|e| format!("Could not publish config: {e}"))?;
        #[cfg(unix)]
        if let Some(dir) = path.parent()
            && let Ok(dir) = std::fs::File::open(dir)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[tauri::command]
pub(crate) fn load_config(state: State<'_, SharedState>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn save_config(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, SharedState>,
    mut config: AppConfig,
) -> Result<AppConfig, String> {
    let write_guard = state
        .config_write
        .lock()
        .map_err(|_| "Settings are unavailable.".to_string())?;
    let previous = state.config.lock().unwrap().clone();
    let window_workspace = workspace_for_window(&state, window.label())?;
    config.base_url = config.base_url.trim().trim_end_matches('/').to_string();
    config.api_key = config.api_key.trim().to_string();
    config.model = config.model.trim().to_string();
    config.theme = match config.theme.trim() {
        theme @ ("light" | "dark" | "auto") => theme.to_string(),
        _ => default_theme(),
    };
    config.summary_model = config.summary_model.trim().to_string();
    config.search_api_key = config.search_api_key.trim().to_string();
    config.search_url = config.search_url.trim().trim_end_matches('/').to_string();
    // The active workspace is window state now. Settings never changes it;
    // opening a workspace updates the default through `open_workspace`.
    config.workspace = previous.workspace.clone();

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
    if config
        .summary_context_window
        .is_some_and(|window| window < 8_192)
    {
        return Err("Summary context window must be at least 8192 tokens.".into());
    }
    if !config.summary_model.is_empty()
        && config.summary_model != config.model
        && config.summary_context_window.is_none()
    {
        return Err("Set the context window for the summary model.".into());
    }
    config.mcp_servers = normalize_mcp_servers(config.mcp_servers)?;
    write_config(&state.config_path, &config)?;
    *state.config.lock().unwrap() = config.clone();
    if previous.allow_all_commands != config.allow_all_commands
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        if config.allow_all_commands {
            granted.insert(window_workspace);
            if let Ok(windows) = state.windows.lock() {
                granted.extend(windows.workspaces().cloned());
            }
        } else {
            granted.clear();
        }
    }
    drop(write_guard);
    let _ = app.emit("config://changed", &config);
    Ok(config)
}

pub(crate) fn remember_default_workspace(
    app: &AppHandle,
    state: &AppState,
    workspace: &Path,
) -> Result<(), String> {
    let path = workspace.to_string_lossy().into_owned();
    let snapshot = {
        let _write = state
            .config_write
            .lock()
            .map_err(|_| "Settings are unavailable.".to_string())?;
        let mut config = state
            .config
            .lock()
            .map_err(|_| "Agent state is unavailable.".to_string())?;
        if crate::paths::same_workspace(Path::new(&config.workspace), workspace) {
            return Ok(());
        }
        let mut snapshot = config.clone();
        snapshot.workspace = path;
        write_config(&state.config_path, &snapshot)?;
        *config = snapshot.clone();
        snapshot
    };
    let _ = app.emit("config://changed", &snapshot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_config_atomically_with_owner_only_permissions() {
        let root = crate::tools::test_support::temp_root("config-atomic");
        let path = root.join("config.json");
        write_config(&path, &AppConfig::default()).unwrap();

        assert!(read_config(&path).is_some());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).ok();
    }

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
    fn configs_saved_before_theme_default_to_auto() {
        let config: AppConfig = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(config.theme, "auto");
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
}
