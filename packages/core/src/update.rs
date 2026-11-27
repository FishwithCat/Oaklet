//! Manual update checks and installation.
//!
//! The updater never runs on startup, on focus, or on a timer. The settings
//! panel invokes these commands, and the update package is verified against the
//! minisign public key compiled into the application before it is installed.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

#[cfg(windows)]
use tauri::Manager;

#[cfg(windows)]
use crate::SharedState;

const RELEASE_URL: &str = "https://github.com/FishwithCat/Oaklet/releases/tag/v";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub version: String,
    pub notes: Option<String>,
    pub release_url: String,
}

#[tauri::command]
pub fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|error| {
            log_failure("could not prepare the updater", &error);
            error.to_string()
        })?
        .check()
        .await
        .map_err(|error| {
            log_failure("could not check for updates", &error);
            error.to_string()
        })?;
    Ok(update.map(|update| UpdateInfo {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        notes: update.body.clone(),
        release_url: release_url(&update.version),
    }))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<String, String> {
    let builder = app.updater_builder();
    #[cfg(windows)]
    let builder = {
        // The NSIS updater exits through std::process::exit, so the normal
        // RunEvent::Exit cleanup never runs. Stop children and settle pending
        // writes before the installer takes over.
        let state = app.state::<SharedState>().inner().clone();
        builder
            .restart_after_install(false)
            .on_before_exit(move || {
                crate::chat::stop_all(&state);
                state.jobs.kill_all();
                crate::architecture::cancel_all(&state);
                // The updater calls this hook on the command's async worker.
                // Drive the drain from a fresh thread so block_on does not
                // re-enter the Tauri runtime.
                let drain_state = state.clone();
                let _ = std::thread::spawn(move || {
                    let _ = tauri::async_runtime::block_on(drain_state.chat.drain(None));
                })
                .join();
                crate::tools::shutdown();
            })
    };
    let updater = builder.build().map_err(|error| {
        log_failure("could not prepare the updater", &error);
        error.to_string()
    })?;
    let update = updater
        .check()
        .await
        .map_err(|error| {
            log_failure("could not check for updates", &error);
            error.to_string()
        })?
        .ok_or_else(|| "Oaklet is already up to date.".to_string())?;
    let version = update.version.clone();
    let bytes = update
        .download(|_chunk, _total| {}, || {})
        .await
        .map_err(|error| {
            log_failure("could not download the update", &error);
            format!("Could not download the update: {error}")
        })?;
    update.install(&bytes).map_err(|error| {
        log_failure("could not install the update", &error);
        format!("Could not install the update: {error}")
    })?;
    Ok(version)
}

fn release_url(version: &str) -> String {
    format!("{RELEASE_URL}{}", version.trim_start_matches('v'))
}

fn log_failure(action: &str, error: &dyn std::fmt::Display) {
    crate::log::key(format_args!("[oaklet] {action}: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_urls_point_at_the_matching_tag() {
        assert_eq!(
            release_url("0.4.0"),
            "https://github.com/FishwithCat/Oaklet/releases/tag/v0.4.0"
        );
        assert_eq!(
            release_url("v0.4.0"),
            "https://github.com/FishwithCat/Oaklet/releases/tag/v0.4.0"
        );
    }
}
