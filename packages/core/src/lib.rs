mod agent;
mod app;
mod architecture;
mod chat;
mod compaction;
mod config;
mod connection;
mod context;
mod files;
mod git;
mod instructions;
mod log;
mod repeat;
mod shutdown;
mod spill;
mod store;
mod terminal;
mod tools;
mod windows;

pub use agent::approval::PendingApproval;
pub use app::{AppState, SharedState};
pub use config::{AppConfig, McpServerConfig, McpTransport};
pub use terminal::TerminalSession;
pub use windows::WindowRegistry;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            windows::handle_second_instance(app, &args);
        }))
        .on_window_event(windows::handle_window_event)
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(app::setup)
        .invoke_handler(tauri::generate_handler![
            config::load_config,
            windows::window_workspace,
            windows::open_workspace,
            shutdown::close_workspace,
            config::save_config,
            connection::test_connection,
            chat::commands::session_create,
            chat::commands::session_list,
            chat::commands::session_get,
            chat::commands::session_send,
            chat::commands::session_stop,
            chat::commands::session_compact,
            chat::commands::session_compact_cancel,
            agent::approval::approve_tool,
            agent::approval::command_allow_all_active,
            agent::approval::revoke_command_allow_all,
            agent::approval::grant_command_allow_all,
            tools::skill::skills_list,
            files::files_list,
            files::files_read,
            files::files_save,
            git::git_log,
            git::git_show,
            git::git_switch,
            git::git_changes,
            git::git_diff,
            architecture::architecture_discover,
            architecture::architecture_load,
            terminal::commands::terminal_open,
            terminal::commands::terminal_write,
            terminal::commands::terminal_ack,
            terminal::commands::terminal_resize,
            terminal::commands::terminal_close,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(shutdown::handle_run_event);
}
