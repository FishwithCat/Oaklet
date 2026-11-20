use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{App, Manager};

use crate::config::{default_workspace, read_config, write_config};
use crate::windows::{canonical_workspace, requested_workspace};
use crate::{
    AppConfig, PendingApproval, TerminalSession, WindowRegistry, architecture, chat, log, shutdown,
    spill, tools,
};

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub exit: Mutex<shutdown::ExitState>,
    pub config: Mutex<AppConfig>,
    pub config_path: PathBuf,
    /// Serializes config file read/modify/write cycles across windows.
    pub config_write: Mutex<()>,
    /// Workspace owned by each webview window. A window's workspace is fixed
    /// for its lifetime, so backend commands never trust a path sent by the
    /// frontend.
    pub windows: Mutex<WindowRegistry>,
    /// App-owned directory for oversized model-facing text.
    pub spill_root: PathBuf,
    pub approvals: Mutex<HashMap<String, PendingApproval>>,
    pub chat: chat::ChatState,
    /// Canonical workspaces where `run_command` approvals were waived for this
    /// session.
    pub command_allow_all: Mutex<HashSet<PathBuf>>,
    /// Shells behind the terminal view, keyed by the frontend's session id.
    pub terminal: Mutex<HashMap<String, TerminalSession>>,
    /// Background commands started through `run_command` with
    /// `run_in_background`; shared across runs and stopped on app exit.
    pub jobs: Arc<tools::jobs::JobRegistry>,
    /// Last valid architecture model and the in-flight compiler generation.
    pub architecture: Mutex<architecture::ArchitectureCache>,
    pub(crate) ids: AtomicU64,
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

pub(crate) fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(log_dir) = app.path().app_log_dir() {
        log::init(log_dir.join("oaklet.log"));
    }
    // The window must not wait for the user's shell startup files. The snapshot
    // is applied to child processes as they are spawned.
    crate::shell_env::start();
    let config_path = app.path().app_config_dir()?.join("config.json");
    let mut config = read_config(&config_path).unwrap_or_default();
    let args: Vec<String> = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let cli_workspace = requested_workspace(&args).and_then(|path| {
        match canonical_workspace(&path.to_string_lossy()) {
            Ok(path) => Some(path),
            Err(error) => {
                log::key(format_args!(
                    "[oaklet] ignoring the command-line workspace: {error}"
                ));
                None
            }
        }
    });
    if config.workspace.trim().is_empty() || !Path::new(&config.workspace).is_dir() {
        config.workspace = default_workspace();
    }
    if let Ok(canonical) = Path::new(&config.workspace).canonicalize() {
        config.workspace = canonical.to_string_lossy().into_owned();
    }
    let cli_workspace = cli_workspace
        .map(|path| {
            let path = path.to_string_lossy().into_owned();
            config.workspace = path.clone();
            path
        })
        .is_some();
    let data_dir = app.path().app_data_dir()?;
    let spill_root = data_dir.join("spill");
    spill::cleanup(&spill_root, spill::RETENTION);
    let chat = chat::ChatState::open(&data_dir.join("oaklet.db")).map_err(std::io::Error::other)?;
    let command_allow_all = if config.allow_all_commands {
        HashSet::from([PathBuf::from(&config.workspace)])
    } else {
        HashSet::new()
    };
    let mut windows = WindowRegistry::default();
    windows
        .register("main".into(), PathBuf::from(&config.workspace))
        .map_err(std::io::Error::other)?;
    let state = Arc::new(AppState {
        exit: Mutex::new(shutdown::ExitState::Running),
        config: Mutex::new(config),
        config_path,
        config_write: Mutex::new(()),
        windows: Mutex::new(windows),
        spill_root,
        approvals: Mutex::new(HashMap::new()),
        chat,
        command_allow_all: Mutex::new(command_allow_all),
        terminal: Mutex::new(HashMap::new()),
        jobs: Arc::new(tools::jobs::JobRegistry::new()),
        architecture: Mutex::new(architecture::ArchitectureCache::default()),
        ids: AtomicU64::new(0),
    });
    app.manage(state.clone());
    // A settled background job reports to the session that started it;
    // the write and the possible new run must not block the job task.
    let notice_app = app.handle().clone();
    let notice_state = state.clone();
    state.jobs.set_notifier(Arc::new(move |job| {
        let app = notice_app.clone();
        let state = notice_state.clone();
        tauri::async_runtime::spawn_blocking(move || {
            chat::job_settled(&app, &state, job);
        });
    }));
    #[cfg(target_os = "macos")]
    shutdown::install_native_quit(app.handle()).map_err(std::io::Error::other)?;
    if cli_workspace {
        let snapshot = state.config.lock().unwrap().clone();
        let _write = state.config_write.lock().unwrap();
        if let Err(error) = write_config(&state.config_path, &snapshot) {
            log::key(format_args!(
                "[oaklet] could not save the command-line workspace: {error}"
            ));
        }
    }
    Ok(())
}
