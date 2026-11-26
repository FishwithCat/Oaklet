use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, State, WebviewWindow, Window, WindowEvent};

use crate::config::remember_default_workspace;
use crate::{AppState, SharedState, architecture, chat, log, shutdown, tools};

const MAX_WORKSPACE_WINDOWS: usize = 8;

#[derive(Default)]
pub struct WindowRegistry {
    by_label: HashMap<String, PathBuf>,
    by_workspace: HashMap<PathBuf, String>,
    last_focused: Option<String>,
}

impl WindowRegistry {
    pub(crate) fn register(&mut self, label: String, workspace: PathBuf) -> Result<(), String> {
        let key = crate::paths::workspace_identity(&workspace);
        if let Some(existing) = self.by_workspace.get(&key)
            && existing != &label
        {
            return Err(format!(
                "That workspace is already open in window {existing}."
            ));
        }
        if let Some(previous) = self.by_label.insert(label.clone(), workspace.clone())
            && self
                .by_workspace
                .get(&crate::paths::workspace_identity(&previous))
                .is_some_and(|existing| existing == &label)
        {
            self.by_workspace
                .remove(&crate::paths::workspace_identity(&previous));
        }
        self.by_workspace.insert(key, label.clone());
        self.last_focused = Some(label);
        Ok(())
    }

    fn remove_label(&mut self, label: &str) -> Option<PathBuf> {
        let workspace = self.by_label.remove(label)?;
        let key = crate::paths::workspace_identity(&workspace);
        if self
            .by_workspace
            .get(&key)
            .is_some_and(|value| value == label)
        {
            self.by_workspace.remove(&key);
        }
        if self.last_focused.as_deref() == Some(label) {
            self.last_focused = self.by_label.keys().next().cloned();
        }
        Some(workspace)
    }

    pub(crate) fn workspace(&self, label: &str) -> Option<&PathBuf> {
        self.by_label.get(label)
    }

    pub(crate) fn workspaces(&self) -> impl Iterator<Item = &PathBuf> {
        self.by_label.values()
    }

    fn label_for(&self, workspace: &Path) -> Option<&str> {
        self.by_workspace
            .get(&crate::paths::workspace_identity(workspace))
            .map(String::as_str)
    }

    fn focus(&mut self, label: &str) {
        if self.by_label.contains_key(label) {
            self.last_focused = Some(label.to_string());
        }
    }

    fn last_label(&self) -> Option<String> {
        self.last_focused
            .clone()
            .or_else(|| self.by_label.keys().next().cloned())
    }
}

pub(crate) fn canonical_workspace(input: &str) -> Result<PathBuf, String> {
    let path = Path::new(input)
        .canonicalize()
        .map_err(|error| format!("Could not open the workspace: {error}"))?;
    if !path.is_dir() {
        return Err("The selected workspace is not a directory.".into());
    }
    Ok(path)
}

pub(crate) fn workspace_for_window(state: &AppState, label: &str) -> Result<PathBuf, String> {
    if let Ok(windows) = state.windows.lock()
        && let Some(workspace) = windows.workspace(label)
    {
        let workspace = workspace.clone();
        if !workspace.is_dir() {
            return Err(format!(
                "Workspace is unavailable: {}",
                crate::paths::display_path(&workspace)
            ));
        }
        return Ok(workspace);
    }

    // The initial window exists before `setup` manages the registry. If a
    // command races that setup, fall back to the configured default for it.
    if label == "main" {
        let workspace = PathBuf::from(
            state
                .config
                .lock()
                .map_err(|_| "Agent state is unavailable.".to_string())?
                .workspace
                .clone(),
        );
        if let Ok(mut windows) = state.windows.lock() {
            let _ = windows.register(label.to_string(), workspace.clone());
        }
        return Ok(workspace);
    }

    Err("This window has no workspace.".into())
}

fn focus_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn focus_last_window(app: &AppHandle, state: &AppState) {
    let label = state
        .windows
        .lock()
        .ok()
        .and_then(|windows| windows.last_label());
    if let Some(label) = label {
        focus_window(app, &label);
    }
}

/// How far a new workspace window steps away from the one it was opened from.
const CASCADE_STEP: i32 = 28;

/// A window or monitor rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Frame {
    fn right(&self) -> i32 {
        self.x + self.width
    }

    fn bottom(&self) -> i32 {
        self.y + self.height
    }
}

/// Place the next window `CASCADE_STEP` down-right from `origin`, restarting at
/// the work area's top-left instead of running past its bottom-right corner. The
/// result stays inside `work_area` even when the window is larger than it.
fn cascade_origin(origin: (i32, i32), size: (i32, i32), work_area: Frame) -> (i32, i32) {
    let stepped = (origin.0 + CASCADE_STEP, origin.1 + CASCADE_STEP);
    let limit = (
        (work_area.right() - size.0).max(work_area.x),
        (work_area.bottom() - size.1).max(work_area.y),
    );
    let next = if stepped.0 <= limit.0 && stepped.1 <= limit.1 {
        stepped
    } else {
        (work_area.x + CASCADE_STEP, work_area.y + CASCADE_STEP)
    };
    (
        next.0.clamp(work_area.x, limit.0),
        next.1.clamp(work_area.y, limit.1),
    )
}

/// Where to place a new workspace window: one cascade step from the window the
/// user is using, inside that window's monitor work area. `None` when the origin
/// window or its monitor cannot be read, so the caller centres instead.
///
/// Call this before registering the new label: `WindowRegistry::register` makes
/// the new label the focused one.
fn cascade_position(app: &AppHandle, state: &AppState) -> Option<(f64, f64)> {
    let label = state.windows.lock().ok()?.last_label()?;
    let origin = app.get_webview_window(&label)?;
    let monitor = origin.current_monitor().ok()??;
    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let to_logical = |physical: i32| (f64::from(physical) / scale).round() as i32;

    let work = monitor.work_area();
    let work_area = Frame {
        x: to_logical(work.position.x),
        y: to_logical(work.position.y),
        width: to_logical(work.size.width as i32),
        height: to_logical(work.size.height as i32),
    };
    let position = origin.outer_position().ok()?;
    let size = origin.outer_size().ok()?;
    let (x, y) = cascade_origin(
        (to_logical(position.x), to_logical(position.y)),
        (
            to_logical(size.width as i32),
            to_logical(size.height as i32),
        ),
        work_area,
    );
    Some((f64::from(x), f64::from(y)))
}

fn open_workspace_window(
    app: &AppHandle,
    state: &AppState,
    workspace: PathBuf,
) -> Result<(), String> {
    if *state.exit.lock().map_err(|_| "App state is unavailable.")? != shutdown::ExitState::Running
    {
        return Err("Oaklet is quitting.".into());
    }
    state.jobs.open_workspace(&workspace);
    let existing = state
        .windows
        .lock()
        .map_err(|_| "Window state is unavailable.".to_string())?
        .label_for(&workspace)
        .map(str::to_string);
    if let Some(label) = existing {
        focus_window(app, &label);
        if let Ok(mut windows) = state.windows.lock() {
            windows.focus(&label);
        }
        if let Err(error) = remember_default_workspace(app, state, &workspace) {
            log::key(format_args!(
                "[oaklet] could not remember the default workspace: {error}"
            ));
        }
        return Ok(());
    }

    // Read the window the user is looking at before the new label takes focus.
    let position = cascade_position(app, state);
    let label = format!("ws-{}", state.next_id());
    {
        let mut windows = state
            .windows
            .lock()
            .map_err(|_| "Window state is unavailable.".to_string())?;
        if let Some(existing) = windows.label_for(&workspace) {
            let existing = existing.to_string();
            drop(windows);
            focus_window(app, &existing);
            if let Err(error) = remember_default_workspace(app, state, &workspace) {
                log::key(format_args!(
                    "[oaklet] could not remember the default workspace: {error}"
                ));
            }
            return Ok(());
        }
        if windows.by_label.len() >= MAX_WORKSPACE_WINDOWS {
            return Err(format!(
                "At most {MAX_WORKSPACE_WINDOWS} workspace windows can be open. Close one first."
            ));
        }
        windows.register(label.clone(), workspace.clone())?;
    }

    let title = crate::paths::display_path(&workspace);
    let mut builder =
        tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("index.html".into()))
            .title(&title)
            .inner_size(1120.0, 760.0)
            .min_inner_size(720.0, 520.0);
    // The main window centres itself; workspace windows cascade instead, so the
    // window they were opened from stays reachable.
    builder = match position {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    let built = builder.build();
    if let Err(error) = built {
        if let Ok(mut windows) = state.windows.lock() {
            windows.remove_label(&label);
        }
        return Err(format!("Could not open a workspace window: {error}"));
    }

    if state
        .config
        .lock()
        .map(|config| config.allow_all_commands)
        .unwrap_or(false)
        && let Ok(mut granted) = state.command_allow_all.lock()
    {
        granted.insert(workspace.clone());
    }
    if let Err(error) = remember_default_workspace(app, state, &workspace) {
        log::key(format_args!(
            "[oaklet] could not remember the default workspace: {error}"
        ));
    }
    focus_window(app, &label);
    if let Ok(mut windows) = state.windows.lock() {
        windows.focus(&label);
    }
    Ok(())
}

pub(crate) fn requested_workspace(args: &[String]) -> Option<PathBuf> {
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--workspace" || arg == "-w" {
            return args.next().map(PathBuf::from);
        }
        if let Some(path) = arg
            .strip_prefix("--workspace=")
            .filter(|path| !path.is_empty())
        {
            return Some(PathBuf::from(path));
        }
        if !arg.starts_with('-') {
            return Some(PathBuf::from(arg));
        }
    }
    None
}

fn cleanup_window(state: &AppState, label: &str) {
    let workspace = state
        .windows
        .lock()
        .ok()
        .and_then(|mut windows| windows.remove_label(label));
    let Some(workspace) = workspace else {
        return;
    };

    chat::stop_window(state, label);
    architecture::cancel_workspace(state, &workspace);
    if let Ok(mut terminals) = state.terminal.lock() {
        terminals.retain(|_, terminal| terminal.window != label);
    }
    state.jobs.close_workspace(&workspace);
    state.jobs.kill_window(label);
    tools::shutdown_workspace(&workspace);
    if let Ok(paths) = state.chat.store().project_worktree_paths(&workspace) {
        for path in paths {
            if state
                .windows
                .lock()
                .is_ok_and(|windows| windows.label_for(&path).is_some())
            {
                continue;
            }
            architecture::cancel_workspace(state, &path);
            tools::shutdown_workspace(&path);
        }
    }
}

#[tauri::command]
pub(crate) fn window_workspace(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<String, String> {
    let workspace = workspace_for_window(&state, window.label())?;
    Ok(workspace.to_string_lossy().into_owned())
}

#[tauri::command]
pub(crate) fn open_workspace(
    app: AppHandle,
    state: State<'_, SharedState>,
    workspace: String,
) -> Result<(), String> {
    let workspace = canonical_workspace(&workspace)?;
    open_workspace_window(&app, &state, workspace)
}

pub(crate) fn handle_second_instance(app: &AppHandle, args: &[String]) {
    let Some(state) = app.try_state::<SharedState>() else {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        return;
    };
    match requested_workspace(args) {
        Some(workspace) => {
            let workspace = workspace.to_string_lossy();
            match canonical_workspace(&workspace) {
                Ok(workspace) => {
                    if let Err(error) = open_workspace_window(app, &state, workspace) {
                        log::key(format_args!(
                            "[oaklet] could not open workspace window: {error}"
                        ));
                        focus_last_window(app, &state);
                    }
                }
                Err(error) => {
                    log::key(format_args!(
                        "[oaklet] could not open requested workspace: {error}"
                    ));
                    focus_last_window(app, &state);
                }
            }
        }
        None => focus_last_window(app, &state),
    }
}

pub(crate) fn handle_window_event(window: &Window, event: &WindowEvent) {
    let Some(state) = window.app_handle().try_state::<SharedState>() else {
        return;
    };
    match event {
        WindowEvent::Focused(true) => {
            if let Ok(mut windows) = state.windows.lock() {
                windows.focus(window.label());
            }
        }
        WindowEvent::Destroyed => {
            cleanup_window(&state, window.label());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_registry_keeps_one_label_per_workspace() {
        let mut windows = WindowRegistry::default();
        windows
            .register("main".into(), PathBuf::from("/tmp/a"))
            .unwrap();
        windows
            .register("ws-1".into(), PathBuf::from("/tmp/b"))
            .unwrap();

        assert!(
            windows
                .register("ws-2".into(), PathBuf::from("/tmp/a"))
                .is_err()
        );
        assert_eq!(windows.label_for(Path::new("/tmp/a")), Some("main"));

        windows.focus("ws-1");
        assert_eq!(windows.last_label().as_deref(), Some("ws-1"));

        assert_eq!(windows.remove_label("main"), Some(PathBuf::from("/tmp/a")));
        assert!(windows.label_for(Path::new("/tmp/a")).is_none());
        assert_eq!(windows.last_label().as_deref(), Some("ws-1"));
    }

    #[test]
    fn parses_a_workspace_from_command_line_arguments() {
        let args = ["oaklet", "--workspace", "/tmp/project"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(
            requested_workspace(&args),
            Some(PathBuf::from("/tmp/project"))
        );

        let args = ["oaklet", "/tmp/positional"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(
            requested_workspace(&args),
            Some(PathBuf::from("/tmp/positional"))
        );

        assert_eq!(requested_workspace(&["oaklet".to_string()]), None);
    }

    fn work_area(width: i32, height: i32) -> Frame {
        Frame {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn cascades_a_new_window_down_and_right() {
        let next = cascade_origin((100, 100), (1120, 760), work_area(1440, 900));
        assert_eq!(next, (100 + CASCADE_STEP, 100 + CASCADE_STEP));
    }

    #[test]
    fn restarts_the_cascade_instead_of_running_off_the_screen() {
        // The window being opened from already sits in the bottom-right corner.
        let next = cascade_origin((320, 140), (1120, 760), work_area(1440, 900));
        assert_eq!(next, (CASCADE_STEP, CASCADE_STEP));
    }

    #[test]
    fn keeps_a_window_larger_than_the_work_area_on_the_screen() {
        // 1120x760 does not fit a 800x600 work area; the origin must not move.
        let next = cascade_origin((0, 0), (1120, 760), work_area(800, 600));
        assert_eq!(next, (0, 0));
    }

    #[test]
    fn cascades_inside_a_monitor_left_of_the_primary_one() {
        let secondary = Frame {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let next = cascade_origin((-1900, 40), (1120, 760), secondary);
        assert_eq!(next, (-1900 + CASCADE_STEP, 40 + CASCADE_STEP));
    }
}
