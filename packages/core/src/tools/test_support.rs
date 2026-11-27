use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;

use super::{
    Access, ApprovalDecision, ApprovalRequest, Approver, CancelHandle, ToolContext, Workspace, jobs,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn temp_root(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("oaklet-{tag}-{}-{stamp}-{id}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

pub fn context(tag: &str) -> (ToolContext, PathBuf, CancelHandle) {
    let root = temp_root(tag);
    let workspace = Workspace::new(root.to_str().unwrap()).unwrap();
    let (handle, cancel) = CancelHandle::new();
    (
        ToolContext {
            workspace,
            cancel,
            access: Access::execute(),
            planning: None,
            plan_validation: None,
            plan_details: None,
            jobs: std::sync::Arc::new(jobs::JobRegistry::new()),
            session_id: "run-test".into(),
            window: "main".into(),
            spill_root: root.join("spill"),
        },
        root,
        handle,
    )
}

pub fn succeed() -> &'static str {
    if cfg!(windows) { "exit /b 0" } else { "true" }
}

pub fn print_directory() -> &'static str {
    if cfg!(windows) { "cd" } else { "pwd" }
}

pub fn exit_with(code: i32) -> String {
    if cfg!(windows) {
        format!("exit /b {code}")
    } else {
        format!("exit {code}")
    }
}

pub fn both_streams_and_fail(code: i32) -> String {
    if cfg!(windows) {
        format!("echo to-stdout& echo to-stderr 1>&2& exit /b {code}")
    } else {
        format!("printf 'to-stdout'; printf 'to-stderr' >&2; exit {code}")
    }
}

pub fn sleep(seconds: u32) -> String {
    if cfg!(windows) {
        format!("ping -n {} 127.0.0.1 >nul", seconds + 1)
    } else {
        format!("sleep {seconds}")
    }
}

pub fn detached_hang() -> &'static str {
    if cfg!(windows) {
        "start /b ping -n 31 127.0.0.1 & echo shell-done"
    } else {
        "sleep 30 & echo shell-done"
    }
}

pub fn detached_hang_silent() -> &'static str {
    if cfg!(windows) {
        "start /b ping -n 31 127.0.0.1 >nul 2>&1 & echo shell-done"
    } else {
        "sleep 30 >/dev/null 2>&1 & echo shell-done"
    }
}

pub(super) struct Allow;

impl Approver for Allow {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async { ApprovalDecision::Allow })
    }
}

#[test]
fn concurrent_temp_roots_with_the_same_tag_are_isolated() {
    use std::collections::HashSet;

    let barrier = std::sync::Barrier::new(3);
    let roots = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    (0..128)
                        .map(|_| {
                            barrier.wait();
                            temp_root("parallel-temp-root")
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    let unique: HashSet<_> = roots.iter().collect();
    for root in &unique {
        std::fs::remove_dir(root).unwrap();
    }
    assert_eq!(unique.len(), roots.len());
}
