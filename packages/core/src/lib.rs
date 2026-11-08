mod agent;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
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
    #[serde(default = "default_workspace")]
    pub workspace: String,
}

fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}

fn default_model() -> String {
    "deepseek-flash".into()
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
    config.workspace = config.workspace.trim().to_string();

    if !(config.base_url.starts_with("http://") || config.base_url.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://.".into());
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
    if previous_workspace != config.workspace
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        *granted = None;
    }
    *state.config.lock().unwrap() = config.clone();
    Ok(config)
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
            app.manage(Arc::new(AppState {
                config: Mutex::new(config),
                config_path,
                approvals: Mutex::new(HashMap::new()),
                runs: Mutex::new(HashMap::new()),
                command_allow_all: Mutex::new(None),
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
            revoke_command_allow_all
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::auto_approves_command;

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
