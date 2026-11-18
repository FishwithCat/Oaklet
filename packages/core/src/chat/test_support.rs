use std::sync::{Arc, Mutex};

use crate::AppConfig;

pub(crate) fn state(
    config: AppConfig,
) -> (crate::SharedState, crate::store::Store, std::path::PathBuf) {
    let root = crate::tools::test_support::temp_root("compaction-run");
    let database = root.join("oaklet.db");
    let store = crate::store::Store::open(&database).unwrap();
    let state = Arc::new(crate::AppState {
        code_index: Arc::new(crate::code_index::CodeIndexManager::new()),
        exit: Mutex::new(crate::shutdown::ExitState::Running),
        config: Mutex::new(config),
        config_path: Default::default(),
        config_write: Mutex::new(()),
        windows: Mutex::new(Default::default()),
        spill_root: Default::default(),
        approvals: Mutex::new(Default::default()),
        chat: crate::chat::ChatState::open(&database).unwrap(),
        command_allow_all: Mutex::new(Default::default()),
        terminal: Mutex::new(Default::default()),
        jobs: Arc::new(crate::tools::jobs::JobRegistry::new()),
        architecture: Mutex::new(Default::default()),
        ids: std::sync::atomic::AtomicU64::new(0),
    });
    (state, store, root)
}
