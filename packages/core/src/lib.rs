mod agent;
mod architecture;
mod git;
mod terminal;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

use tools::CancelHandle;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub config_path: PathBuf,
    pub approvals: Mutex<HashMap<String, PendingApproval>>,
    pub runs: Mutex<HashMap<String, CancelHandle>>,
    /// Canonical workspace where `run_command` approvals were waived for this
    /// session. `None` means every command is confirmed.
    pub command_allow_all: Mutex<Option<PathBuf>>,
    /// Shells behind the terminal view, keyed by the frontend's session id.
    pub terminal: Mutex<HashMap<String, terminal::Session>>,
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
    #[serde(default = "default_search_url")]
    pub search_url: String,
    #[serde(default = "default_workspace")]
    pub workspace: String,
}

fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}

fn default_model() -> String {
    "deepseek-flash".into()
}

fn default_search_url() -> String {
    tools::web::DEFAULT_ENDPOINT.into()
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
            search_url: default_search_url(),
            workspace: default_workspace(),
        }
    }
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
    if !Path::new(&config.workspace).is_dir() {
        return Err(format!(
            "Workspace is not a directory: {}",
            config.workspace
        ));
    }
    if let Ok(canonical) = Path::new(&config.workspace).canonicalize() {
        config.workspace = canonical.to_string_lossy().into_owned();
    }

    let previous_workspace = state.config.lock().unwrap().workspace.clone();
    write_config(&state.config_path, &config)?;
    *state.config.lock().unwrap() = config.clone();
    if previous_workspace != config.workspace
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        *granted = None;
    }
    if previous_workspace != config.workspace {
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
fn agent_send(
    app: AppHandle,
    state: State<'_, SharedState>,
    messages: Vec<serde_json::Value>,
) -> Result<String, String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    if !Path::new(&config.workspace).is_dir() {
        return Err("Choose an existing workspace folder first.".into());
    }

    let run_id = state.next_id();
    let (cancel_handle, cancel) = CancelHandle::new();
    state
        .runs
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .insert(run_id.clone(), cancel_handle);

    let shared = state.inner().clone();
    let id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        agent::run(app, shared.clone(), id.clone(), messages, config, cancel).await;
        if let Ok(mut runs) = shared.runs.lock() {
            runs.remove(&id);
        }
    });

    Ok(run_id)
}

#[tauri::command]
fn agent_stop(state: State<'_, SharedState>, run_id: String) {
    if let Ok(runs) = state.runs.lock()
        && let Some(cancel) = runs.get(&run_id)
    {
        cancel.cancel();
    }

    let mut approvals = match state.approvals.lock() {
        Ok(approvals) => approvals,
        Err(_) => return,
    };
    let ids: Vec<String> = approvals
        .iter()
        .filter(|(_, pending)| pending.run_id == run_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some(pending) = approvals.remove(&id) {
            let _ = pending.sender.send(false);
        }
    }
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
async fn git_log(state: State<'_, SharedState>) -> Result<git::GitLog, String> {
    let workspace = workspace_path(&state)?;
    tauri::async_runtime::spawn_blocking(move || git::log(&workspace))
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
            app.manage(Arc::new(AppState {
                config: Mutex::new(config),
                config_path,
                approvals: Mutex::new(HashMap::new()),
                runs: Mutex::new(HashMap::new()),
                command_allow_all: Mutex::new(None),
                terminal: Mutex::new(HashMap::new()),
                architecture: Mutex::new(architecture::ArchitectureCache::default()),
                ids: AtomicU64::new(0),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            agent_send,
            agent_stop,
            approve_tool,
            command_allow_all_active,
            revoke_command_allow_all,
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
            }
        });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{AppConfig, auto_approves_command};

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
