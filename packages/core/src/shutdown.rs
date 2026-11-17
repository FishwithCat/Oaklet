use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use crate::{SharedState, WindowRegistry, architecture, chat, tools};

#[derive(Default, PartialEq)]
pub enum ExitState {
    #[default]
    Running,
    Draining,
    Ready,
}

#[tauri::command]
pub async fn close_workspace(
    window: WebviewWindow,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let workspace = workspace_to_close(&state.windows, window.label())?;
    chat::stop_window(&state, window.label());
    state.jobs.close_workspace(&workspace);
    state.jobs.kill_workspace(&workspace);
    architecture::cancel_workspace(&state, &workspace);
    state.chat.drain(Some(window.label())).await?;
    state.jobs.wait_for_exit(Some(&workspace)).await;
    Ok(())
}

fn workspace_to_close(
    windows: &Mutex<WindowRegistry>,
    label: &str,
) -> Result<std::path::PathBuf, String> {
    // Cleanup uses the original binding even if the directory has disappeared.
    windows
        .lock()
        .map_err(|_| "Window state is unavailable.".to_string())?
        .workspace(label)
        .cloned()
        .ok_or_else(|| "This window has no workspace.".into())
}

pub fn request_exit(app: &AppHandle, api: &tauri::ExitRequestApi) {
    let state = app.state::<SharedState>();
    if *state.exit.lock().unwrap() == ExitState::Ready {
        return;
    }
    api.prevent_exit();
    let windows = app.webview_windows();
    if !windows.is_empty() {
        for label in windows.keys() {
            if let Err(error) = app.emit_to(label, "app://quit_requested", ()) {
                crate::log::key(format_args!(
                    "[oaklet] could not request window close: {error}"
                ));
            }
        }
        return;
    }
    if !begin_drain(&state.exit) {
        return;
    }
    let app = app.clone();
    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        chat::stop_all(&state);
        state.jobs.kill_all();
        architecture::cancel_all(&state);
        if let Err(error) = state.chat.drain(None).await {
            *state.exit.lock().unwrap() = ExitState::Running;
            app.dialog().message(format!("Could not save chats: {error}\nOaklet is still running. Fix the problem and try Quit again."))
                .title("Could not quit Oaklet").show(|_| {});
            return;
        }
        state.jobs.wait_for_exit(None).await;
        tools::shutdown();
        *state.exit.lock().unwrap() = ExitState::Ready;
        app.exit(0);
    });
}

fn begin_drain(exit: &Mutex<ExitState>) -> bool {
    let mut state = exit.lock().unwrap();
    if *state != ExitState::Running {
        return false;
    }
    *state = ExitState::Draining;
    true
}

// Cocoa's native Quit bypasses Tauri's ExitRequested; return Cancel and use its guarded exit path.
#[cfg(target_os = "macos")]
pub fn install_native_quit(app: &AppHandle) -> Result<(), String> {
    use std::ffi::{c_char, c_void};
    use std::sync::OnceLock;

    static APP: OnceLock<AppHandle> = OnceLock::new();
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        fn object_getClass(object: *mut c_void) -> *mut c_void;
        fn class_addMethod(
            class: *mut c_void,
            selector: *mut c_void,
            implementation: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> usize,
            types: *const c_char,
        ) -> bool;
    }
    extern "C" fn should_terminate(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> usize {
        if let Some(app) = APP.get() {
            app.exit(0);
        }
        0 // NSTerminateCancel; the asynchronous close flow exits when every window agrees.
    }
    APP.set(app.clone())
        .map_err(|_| "Native Quit handler is already installed.")?;
    // Setup runs on the main thread; these selectors have no arguments and return Objective-C objects.
    let installed = unsafe {
        let application = objc_msgSend(
            objc_getClass(c"NSApplication".as_ptr()),
            sel_registerName(c"sharedApplication".as_ptr()),
        );
        let delegate = objc_msgSend(application, sel_registerName(c"delegate".as_ptr()));
        !delegate.is_null()
            && class_addMethod(
                object_getClass(delegate),
                sel_registerName(c"applicationShouldTerminate:".as_ptr()),
                should_terminate,
                c"Q@:@".as_ptr(),
            )
    };
    if installed {
        Ok(())
    } else {
        Err("Could not install the native Quit handler.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_workspace_can_still_be_closed() {
        let root = crate::tools::test_support::temp_root("close-removed-workspace");
        let mut windows = WindowRegistry::default();
        windows.register("main".into(), root.clone()).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let windows = Mutex::new(windows);
        assert_eq!(workspace_to_close(&windows, "main").unwrap(), root);
        assert!(workspace_to_close(&windows, "other-window").is_err());
    }

    #[test]
    fn repeated_exit_requests_cannot_start_multiple_drains() {
        let exit = Mutex::new(ExitState::Running);
        assert!(begin_drain(&exit));
        assert!(!begin_drain(&exit));
        *exit.lock().unwrap() = ExitState::Ready;
        assert!(!begin_drain(&exit));
    }
}
