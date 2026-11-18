use super::language_servers::{LanguageServerManager, ProjectKey};
use super::*;
use crate::tools::{CancelHandle, test_support::temp_root};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        let root = temp_root(tag);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            format!("pub fn alive() {{}}\n{}", "\n".repeat(99)),
        )
        .unwrap();
        Self(root)
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.0.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn count(&self, event: &str) -> usize {
        self.events()
            .iter()
            .filter(|value| value["event"] == event)
            .count()
    }
    fn command(&self) -> ServerCommand {
        ServerCommand {
            program: super::runtime::find_program(
                "python3",
                &self.0,
                std::env::var_os("PATH").as_deref(),
            )
            .unwrap(),
            args: vec![
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/code_index/fake_server.py")
                    .to_string_lossy()
                    .into_owned(),
            ],
            version: "fake-1".into(),
            runtime_versions: vec![],
            typescript_sdk: None,
        }
    }
    fn config(&self) -> CodeIndexConfig {
        let command = self.command();
        CodeIndexConfig {
            servers: BTreeMap::from([(
                Language::Rust,
                ServerConfig {
                    enabled: true,
                    command: Some(command.program.to_string_lossy().into_owned()),
                    args: command.args,
                },
            )]),
        }
    }
    fn register(&self, pool: &LanguageServerManager) -> ProjectKey {
        pool.register(&self.0, &self.0, Language::Rust, self.command())
    }
    #[cfg(unix)]
    fn assert_reaped(&self) {
        for event in self
            .events()
            .iter()
            .filter(|event| event["event"] == "spawn")
        {
            let pid = event["pid"].as_i64().unwrap() as i32;
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "process {pid} remains alive"
            );
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !predicate() {
        assert!(Instant::now() < deadline, "lifecycle condition timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn symbols() -> Query {
    Query::Symbols {
        file: "src/lib.rs".into(),
    }
}
fn definition(line: u32) -> Query {
    Query::Definition {
        file: "src/lib.rs".into(),
        line,
        column: 0,
    }
}
fn assert_symbol(result: QueryResult) {
    assert!(!result.completeness_known);
    match result.data {
        QueryData::Symbols(items) => assert_eq!(items[0].name, "alive"),
        _ => panic!("wrong query result"),
    }
}
async fn ready(manager: &CodeIndexManager, root: &Path) {
    until(|| {
        manager.status(root).is_ok_and(|status| {
            !status.discovering
                && !status.stale
                && status.projects.len() == 1
                && status.projects[0].state == ServerState::Ready
        })
    })
    .await;
}

#[tokio::test]
async fn service_deduplicates_startup_and_isolates_workspaces_and_reopen() {
    let a = Fixture::new("index-service-a");
    let b = Fixture::new("index-service-b");
    let manager = Arc::new(CodeIndexManager::new());
    for _ in 0..8 {
        manager.open_workspace(&a.0, a.config()).unwrap();
    }
    manager.open_workspace(&b.0, b.config()).unwrap();
    ready(&manager, &a.0).await;
    ready(&manager, &b.0).await;
    let (_owner, cancel) = CancelHandle::new();
    for result in
        futures_util::future::join_all((0..16).map(|_| manager.query(&a.0, symbols(), &cancel)))
            .await
    {
        assert_symbol(result.unwrap());
    }
    assert_eq!(a.count("spawn"), 1);
    assert_eq!(b.count("spawn"), 1);
    assert!(matches!(
        manager
            .query(
                &a.0,
                Query::Symbols {
                    file: b.0.join("src/lib.rs").to_string_lossy().into_owned()
                },
                &cancel
            )
            .await,
        Err(IndexError::Invalid(_))
    ));
    let generation = manager.status(&a.0).unwrap().projects[0].generation;
    manager.close_workspace(&a.0);
    tokio::join!(manager.drain(), manager.drain());
    assert_eq!(manager.status(&a.0).unwrap_err(), IndexError::Closed);
    assert_symbol(manager.query(&b.0, symbols(), &cancel).await.unwrap());
    manager.open_workspace(&a.0, a.config()).unwrap();
    ready(&manager, &a.0).await;
    assert!(manager.status(&a.0).unwrap().projects[0].generation > generation);
    manager.shutdown().await;
    assert_eq!(
        manager.open_workspace(&a.0, a.config()).unwrap_err(),
        IndexError::Closed
    );
    assert_eq!(a.count("exit"), 2);
    assert_eq!(b.count("exit"), 1);
    #[cfg(unix)]
    {
        a.assert_reaped();
        b.assert_reaped();
    }
}

#[tokio::test]
async fn cancelling_a_query_keeps_server_and_other_calls_alive() {
    let fixture = Fixture::new("index-cancel");
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let (owner, cancel) = CancelHandle::new();
    let root = fixture.0.clone();
    let active = manager.clone();
    let pending = tokio::spawn(async move { active.query(&root, definition(0), &cancel).await });
    until(|| fixture.count("edit-rejected") == 1).await;
    let status = manager.status(&fixture.0).unwrap();
    assert_eq!(status.projects[0].progress, vec!["Analyzing saved sources"]);
    assert!(!status.projects[0].capabilities.as_ref().unwrap().calls);
    owner.cancel();
    assert_eq!(pending.await.unwrap().unwrap_err(), IndexError::Cancelled);
    until(|| fixture.count("cancel") == 1).await;
    crate::tools::shutdown_workspace(&fixture.0);
    let (_owner, cancel) = CancelHandle::new();
    assert_symbol(manager.query(&fixture.0, symbols(), &cancel).await.unwrap());
    assert_eq!(fixture.count("spawn"), 1);
    assert!(matches!(
        manager
            .query(
                &fixture.0,
                Query::Calls {
                    file: "src/lib.rs".into(),
                    line: 0,
                    column: 0,
                    incoming: true
                },
                &cancel
            )
            .await,
        Err(IndexError::Unsupported(_))
    ));
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn crashes_fail_current_call_and_restart_only_on_demand() {
    let fixture = Fixture::new("index-crash");
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let generation = manager.status(&fixture.0).unwrap().projects[0].generation;
    let (_owner, cancel) = CancelHandle::new();
    assert!(matches!(
        manager.query(&fixture.0, definition(99), &cancel).await,
        Err(IndexError::Backend(_))
    ));
    assert!(matches!(
        manager.status(&fixture.0).unwrap().projects[0].state,
        ServerState::Failed(_)
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(fixture.count("spawn"), 1);
    assert_symbol(manager.query(&fixture.0, symbols(), &cancel).await.unwrap());
    assert!(manager.status(&fixture.0).unwrap().projects[0].generation > generation);
    manager
        .retry(&fixture.0, &fixture.0, Language::Rust, &cancel)
        .await
        .unwrap();
    assert_eq!(fixture.count("spawn"), 3);
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn malformed_frames_fail_without_allocating_the_advertised_size() {
    let fixture = Fixture::new("index-bad-frame");
    let pool = LanguageServerManager::new();
    let key = fixture.register(&pool);
    let (_owner, cancel) = CancelHandle::new();
    assert_symbol(
        pool.query(&key, &fixture.0.join("src/lib.rs"), &symbols(), &cancel)
            .await
            .unwrap(),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        pool.query(
            &key,
            &fixture.0.join("src/lib.rs"),
            &Query::References {
                file: "src/lib.rs".into(),
                line: 0,
                column: 0,
            },
            &cancel,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(IndexError::Backend(_))));
    pool.close_workspace(&fixture.0).await.unwrap();
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn closing_during_initialize_and_discovery_cannot_resurrect_servers() {
    let fixture = Fixture::new("index-close-start");
    let manager = Arc::new(CodeIndexManager::new());
    std::fs::write(fixture.0.join("hang-init"), "").unwrap();
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    until(|| fixture.count("initialize") == 1).await;
    manager.close_workspace(&fixture.0);
    tokio::time::timeout(Duration::from_secs(12), manager.drain())
        .await
        .unwrap();
    assert_eq!(manager.status(&fixture.0).unwrap_err(), IndexError::Closed);
    std::fs::remove_file(fixture.0.join("hang-init")).unwrap();
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    manager.close_workspace(&fixture.0);
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    assert_eq!(fixture.count("spawn"), 2);
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn pool_prioritizes_queries_without_killing_an_active_request() {
    let a = Fixture::new("index-pool-a");
    let b = Fixture::new("index-pool-b");
    let c = Fixture::new("index-pool-c");
    let pool = LanguageServerManager::with_limits(1, Duration::from_secs(120));
    let ka = a.register(&pool);
    let kb = b.register(&pool);
    let kc = c.register(&pool);
    let (owner, cancel) = CancelHandle::new();
    let manager = pool.clone();
    let path = a.0.join("src/lib.rs");
    let active =
        tokio::spawn(async move { manager.query(&ka, &path, &definition(0), &cancel).await });
    until(|| a.count("query") == 1).await;
    pool.start(kb.clone());
    let (_other_owner, other_cancel) = CancelHandle::new();
    let manager = pool.clone();
    let path = c.0.join("src/lib.rs");
    let waiting =
        tokio::spawn(async move { manager.query(&kc, &path, &symbols(), &other_cancel).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(b.count("spawn"), 0);
    assert_eq!(c.count("spawn"), 0);
    assert_eq!(pool.status(&kb).unwrap().state, ServerState::Queued);
    owner.cancel();
    assert_eq!(active.await.unwrap().unwrap_err(), IndexError::Cancelled);
    assert_symbol(
        tokio::time::timeout(Duration::from_secs(8), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(a.count("shutdown"), 1);
    until(|| b.count("ready") == 1).await;
    for root in [&a.0, &b.0, &c.0] {
        pool.close_workspace(root).await.unwrap();
    }
    #[cfg(unix)]
    {
        a.assert_reaped();
        b.assert_reaped();
        c.assert_reaped();
    }
}

#[tokio::test]
async fn idle_reclamation_and_startup_failure_release_capacity() {
    let bad = Fixture::new("index-pool-fail");
    let good = Fixture::new("index-pool-idle");
    let pool = LanguageServerManager::with_limits(1, Duration::ZERO);
    let mut command = bad.command();
    command.program = bad.0.join("does-not-exist");
    let failed = pool.register(&bad.0, &bad.0, Language::Rust, command);
    let key = good.register(&pool);
    pool.start(failed.clone());
    pool.start(key.clone());
    until(|| pool.status(&key).unwrap().state == ServerState::Ready).await;
    assert!(matches!(
        pool.status(&failed).unwrap().state,
        ServerState::Failed(_)
    ));
    pool.reap_idle().await;
    assert_eq!(pool.status(&key).unwrap().state, ServerState::Idle);
    assert_eq!(good.count("exit"), 1);
    for root in [&bad.0, &good.0] {
        pool.close_workspace(root).await.unwrap();
    }
    #[cfg(unix)]
    good.assert_reaped();
}

#[cfg(unix)]
#[tokio::test]
async fn retry_detects_a_previously_missing_server_and_disabled_projects_stay_disabled() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new("index-retry-missing");
    let disabled = Fixture::new("index-disabled");
    let manager = Arc::new(CodeIndexManager::new());
    let mut config = fixture.config();
    let program = fixture.command().program;
    let installed = fixture.0.join("bin/python3");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    config.servers.get_mut(&Language::Rust).unwrap().command =
        Some(installed.to_string_lossy().into_owned());
    manager.open_workspace(&fixture.0, config).unwrap();
    let mut config = disabled.config();
    config.servers.get_mut(&Language::Rust).unwrap().enabled = false;
    manager.open_workspace(&disabled.0, config).unwrap();
    until(|| {
        manager.status(&fixture.0).is_ok_and(|status| {
            status
                .projects
                .first()
                .is_some_and(|project| matches!(project.state, ServerState::Failed(_)))
        })
    })
    .await;
    until(|| {
        manager.status(&disabled.0).is_ok_and(|status| {
            !status.stale
                && status
                    .projects
                    .first()
                    .is_some_and(|project| project.state == ServerState::Disabled)
        })
    })
    .await;
    let (_owner, cancel) = CancelHandle::new();
    assert_eq!(
        manager
            .query(&disabled.0, symbols(), &cancel)
            .await
            .unwrap_err(),
        IndexError::Disabled
    );
    symlink(program, installed).unwrap();
    manager
        .retry(&fixture.0, &fixture.0, Language::Rust, &cancel)
        .await
        .unwrap();
    assert_symbol(manager.query(&fixture.0, symbols(), &cancel).await.unwrap());
    assert_eq!(disabled.count("spawn"), 0);
    manager.shutdown().await;
    fixture.assert_reaped();
}

#[cfg(unix)]
#[tokio::test]
async fn forced_shutdown_kills_the_language_server_process_group() {
    let fixture = Fixture::new("index-force-shutdown");
    std::fs::write(fixture.0.join("spawn-child"), "").unwrap();
    std::fs::write(fixture.0.join("hang-shutdown"), "").unwrap();
    let pool = LanguageServerManager::new();
    let key = fixture.register(&pool);
    let (_owner, cancel) = CancelHandle::new();
    assert_symbol(
        pool.query(&key, &fixture.0.join("src/lib.rs"), &symbols(), &cancel)
            .await
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(6), pool.close_workspace(&fixture.0))
        .await
        .unwrap()
        .unwrap();
    fixture.assert_reaped();
    let pid = fixture
        .events()
        .iter()
        .find(|event| event["event"] == "descendant")
        .unwrap()["pid"]
        .as_i64()
        .unwrap() as i32;
    until(|| unsafe { libc::kill(pid, 0) } == -1).await;
}

#[tokio::test]
async fn simultaneous_first_queries_share_one_startup() {
    let fixture = Fixture::new("index-first-query");
    let pool = LanguageServerManager::new();
    let key = fixture.register(&pool);
    let (_owner, cancel) = CancelHandle::new();
    let path = fixture.0.join("src/lib.rs");
    let query = symbols();
    for result in
        futures_util::future::join_all((0..16).map(|_| pool.query(&key, &path, &query, &cancel)))
            .await
    {
        assert_symbol(result.unwrap());
    }
    assert_eq!(fixture.count("spawn"), 1);
    pool.close_workspace(&fixture.0).await.unwrap();
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn saved_changes_atomic_rename_delete_and_new_files_update_without_queries() {
    let fixture = Fixture::new("index-saved-files");
    let manifest = std::fs::read_to_string(fixture.0.join("Cargo.toml")).unwrap();
    std::fs::write(
        fixture.0.join("Cargo.toml"),
        format!("{manifest}\n[lib]\npath = \"src/entry.rs\"\n"),
    )
    .unwrap();
    std::fs::write(fixture.0.join("src/entry.rs"), "mod lib;\n").unwrap();
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let (_owner, cancel) = CancelHandle::new();
    assert_symbol(manager.query(&fixture.0, symbols(), &cancel).await.unwrap());
    until(|| fixture.count("watch-registered") == 1).await;
    let saved = "pub fn alive() { let _ = \"😀中\"; }\r\n";
    std::fs::write(fixture.0.join("src/lib.rs.tmp"), saved).unwrap();
    std::fs::rename(
        fixture.0.join("src/lib.rs.tmp"),
        fixture.0.join("src/lib.rs"),
    )
    .unwrap();
    // No query triggers this update: the native watcher must deliver it.
    until(|| {
        fixture
            .events()
            .iter()
            .any(|event| event["event"] == "change" && event["document"]["text"] == saved)
    })
    .await;
    until(|| fixture.count("files") >= 1 && fixture.count("save") == 1).await;
    until(|| manager.status(&fixture.0).is_ok_and(|s| !s.stale)).await;
    assert_eq!(fixture.count("spawn"), 1);
    let opened = fixture.count("open");
    assert_symbol(manager.query(&fixture.0, symbols(), &cancel).await.unwrap());
    assert_eq!(fixture.count("open"), opened);
    std::fs::rename(fixture.0.join("src/lib.rs"), fixture.0.join("src/moved.rs")).unwrap();
    until(|| fixture.count("close") == 1).await;
    until(|| manager.status(&fixture.0).is_ok_and(|s| !s.stale)).await;
    assert!(matches!(
        manager.query(&fixture.0, symbols(), &cancel).await,
        Err(IndexError::Invalid(_))
    ));
    assert_symbol(
        manager
            .query(
                &fixture.0,
                Query::Symbols {
                    file: "src/moved.rs".into(),
                },
                &cancel,
            )
            .await
            .unwrap(),
    );
    std::fs::remove_file(fixture.0.join("src/moved.rs")).unwrap();
    until(|| fixture.count("close") == 2).await;
    std::fs::write(fixture.0.join("src/new.rs"), "pub fn alive() {}\n").unwrap();
    until(|| {
        manager
            .status(&fixture.0)
            .is_ok_and(|s| !s.stale && s.revision >= 4)
    })
    .await;
    // Discovery can be pending in the 200 ms merge window: wait for routing, not a guessed delay.
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        assert!(Instant::now() < deadline);
        match manager
            .query(
                &fixture.0,
                Query::Symbols {
                    file: "src/new.rs".into(),
                },
                &cancel,
            )
            .await
        {
            Ok(result) => {
                assert_symbol(result);
                break;
            }
            Err(IndexError::NotReady | IndexError::Invalid(_)) => {
                tokio::time::sleep(Duration::from_millis(30)).await
            }
            Err(error) => panic!("{error}"),
        }
    }
    let generation = manager.status(&fixture.0).unwrap().projects[0].generation;
    std::fs::write(fixture.0.join(".ignore"), "src/new.rs\n").unwrap();
    until(|| {
        manager
            .status(&fixture.0)
            .is_ok_and(|s| !s.stale && s.projects[0].generation > generation)
    })
    .await;
    assert!(matches!(
        manager
            .query(
                &fixture.0,
                Query::Symbols {
                    file: "src/new.rs".into()
                },
                &cancel
            )
            .await,
        Err(IndexError::Invalid(_))
    ));
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn full_sync_and_utf8_positions_use_saved_content_and_reject_invalid_input() {
    let fixture = Fixture::new("index-utf8-full-sync");
    std::fs::write(
        fixture.0.join("sync-options.json"),
        r#"{"encoding":"utf-8","syncKind":1,"unicodeResult":true}"#,
    )
    .unwrap();
    std::fs::write(fixture.0.join("src/lib.rs"), "😀alive\r\n").unwrap();
    let pool = LanguageServerManager::new();
    let key = fixture.register(&pool);
    let (_owner, cancel) = CancelHandle::new();
    let result = pool
        .query(&key, &fixture.0.join("src/lib.rs"), &symbols(), &cancel)
        .await
        .unwrap();
    match result.data {
        QueryData::Symbols(items) => {
            assert_eq!(items[0].location.column, 2);
            assert_eq!(items[0].location.end_column, 7);
        }
        _ => panic!("Wrong result"),
    }
    std::fs::write(fixture.0.join("src/lib.rs"), "😀alive changed\r\n").unwrap();
    pool.query(&key, &fixture.0.join("src/lib.rs"), &symbols(), &cancel)
        .await
        .unwrap();
    assert_eq!(fixture.count("change"), 1);
    assert!(matches!(
        pool.query(
            &key,
            &fixture.0.join("src/lib.rs"),
            &Query::Definition {
                file: "src/lib.rs".into(),
                line: 0,
                column: 1
            },
            &cancel
        )
        .await,
        Err(IndexError::Invalid(_))
    ));
    assert_eq!(fixture.count("query"), 0);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", fixture.0.join("src/escape.rs")).unwrap();
        assert!(matches!(
            pool.query(&key, &fixture.0.join("src/escape.rs"), &symbols(), &cancel)
                .await,
            Err(IndexError::Invalid(_))
        ));
    }
    pool.close_workspace(&fixture.0).await.unwrap();
}

#[tokio::test]
async fn configuration_restarts_the_project_and_startup_changes_are_discovered() {
    let fixture = Fixture::new("index-config-refresh");
    let independent = fixture.0.join("independent");
    std::fs::create_dir_all(independent.join("src")).unwrap();
    std::fs::write(
        independent.join("Cargo.toml"),
        "[package]\nname = \"independent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(independent.join("src/lib.rs"), "pub fn alive() {}\n").unwrap();
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    std::fs::write(
        fixture.0.join("src/during-startup.rs"),
        "pub fn alive() {}\n",
    )
    .unwrap();
    until(|| {
        manager.status(&fixture.0).is_ok_and(|s| {
            !s.stale
                && s.projects.len() == 2
                && s.projects.iter().all(|p| p.state == ServerState::Ready)
        })
    })
    .await;
    let independent_generation = manager
        .status(&fixture.0)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.root == independent)
        .unwrap()
        .generation;
    let (_owner, cancel) = CancelHandle::new();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if manager
            .query(
                &fixture.0,
                Query::Symbols {
                    file: "src/during-startup.rs".into(),
                },
                &cancel,
            )
            .await
            .is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let generation = manager.status(&fixture.0).unwrap().projects[0].generation;
    let manifest = std::fs::read_to_string(fixture.0.join("Cargo.toml")).unwrap();
    std::fs::write(
        fixture.0.join("Cargo.toml"),
        format!("{manifest}\n# changed externally\n"),
    )
    .unwrap();
    until(|| {
        manager.status(&fixture.0).is_ok_and(|s| {
            !s.stale
                && s.projects[0].state == ServerState::Ready
                && s.projects[0].generation > generation
        })
    })
    .await;
    assert_eq!(fixture.count("spawn"), 2);
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .projects
            .iter()
            .find(|p| p.root == independent)
            .unwrap()
            .generation,
        independent_generation
    );
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}

#[tokio::test]
async fn registered_non_source_files_preserve_create_change_delete_kinds() {
    let fixture = Fixture::new("index-registered-data");
    std::fs::write(fixture.0.join("sync-options.json"), r#"{"watchData":true}"#).unwrap();
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    until(|| fixture.count("watch-registered") == 1).await;
    let path = fixture.0.join("schema.oaklet-data");
    let uri = lsp_types::Url::from_file_path(&path).unwrap().to_string();
    for kind in 1..=3 {
        if kind == 3 {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::write(&path, kind.to_string()).unwrap();
        }
        until(|| {
            fixture.events().iter().any(|event| {
                event["event"] == "files"
                    && event["changes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|change| change["uri"] == uri && change["type"] == kind)
            })
        })
        .await;
    }
    manager.shutdown().await;
}

async fn persisted(manager: &CodeIndexManager, root: &Path, expected: usize) {
    until(|| {
        manager
            .status(root)
            .ok()
            .and_then(|s| s.persistence)
            .is_some_and(|s| {
                !s.verifying && !s.extracting && s.files == expected && s.error.is_none()
            })
    })
    .await;
}

#[tokio::test]
async fn sqlite_reopen_reuses_hashes_rebuilds_only_changes_and_prunes_deleted_files() {
    let fixture = Fixture::new("index-persistence");
    let database = temp_root("index-persistence-db");
    let path = database.join("oaklet.db");
    std::fs::write(fixture.0.join("src/other.rs"), "pub fn other() {}\n").unwrap();
    let store = crate::store::Store::open(&path).unwrap();
    store
        .create_session("preserved", &fixture.0.to_string_lossy(), 1)
        .unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 2).await;
    let first = manager.status(&fixture.0).unwrap().persistence.unwrap();
    assert_eq!(first.extracted_files, 2);
    assert!(!first.ipc_covered);
    assert_eq!(fixture.count("symbols"), 2);
    assert_eq!(fixture.count("save"), 0, "initial didOpen is not a save");
    assert_eq!(fixture.count("open"), fixture.count("close"));
    let original = store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0]
        .snapshot
        .clone();
    manager.shutdown().await;
    drop(manager);
    drop(store);

    let store = crate::store::Store::open(&path).unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 2).await;
    let restored = manager.status(&fixture.0).unwrap().persistence.unwrap();
    assert_eq!(restored.restored_projects, 1);
    assert_eq!(restored.reused_files, 2);
    assert_eq!(
        fixture.count("symbols"),
        2,
        "unchanged files must not reach the server"
    );
    assert_eq!(restored.projects[0].snapshot, original);

    std::fs::write(fixture.0.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
    until(|| fixture.count("symbols") == 3).await;
    persisted(&manager, &fixture.0, 2).await;
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .persistence
            .unwrap()
            .reused_files,
        1
    );
    assert_ne!(
        store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0].snapshot,
        original
    );
    std::fs::remove_file(fixture.0.join("src/other.rs")).unwrap();
    until(|| manager.status(&fixture.0).unwrap().revision >= 2).await;
    persisted(&manager, &fixture.0, 1).await;
    let id = store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0].id;
    assert_eq!(
        store
            .index_files(id)
            .unwrap()
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["src/lib.rs"]
    );
    assert_eq!(fixture.count("symbols"), 3);
    assert!(store.get_session("preserved").unwrap().is_some());
    manager.shutdown().await;
    fixture.assert_reaped();
    drop(manager);
    drop(store);
    let source = fixture.0.join("src/lib.rs");
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let size = std::fs::metadata(&source).unwrap().len();
    std::fs::write(&source, "pub fn renamed() {}\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(std::fs::metadata(&source).unwrap().len(), size);
    assert_eq!(
        std::fs::metadata(&source).unwrap().modified().unwrap(),
        modified
    );
    let store = crate::store::Store::open(&path).unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    assert_eq!(
        fixture.count("symbols"),
        4,
        "equal mtime/size must not hide changed contents"
    );
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .persistence
            .unwrap()
            .reused_files,
        0
    );
    manager.shutdown().await;
    std::fs::remove_dir_all(database).unwrap();
}

#[tokio::test]
async fn background_index_yields_to_queries_and_does_not_hold_the_database_lock() {
    let fixture = Fixture::new("index-background-priority");
    std::fs::write(fixture.0.join("hang-symbols"), "").unwrap();
    let store = crate::store::Store::in_memory().unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    until(|| fixture.count("symbols") > 0).await;
    let database = store.clone();
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || database.create_session("unblocked", "/chat", 1)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    std::fs::remove_file(fixture.0.join("hang-symbols")).unwrap();
    let (_owner, cancel) = CancelHandle::new();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        manager.query(&fixture.0, symbols(), &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(result.data,QueryData::Symbols(items) if items.len()==1));
    until(|| fixture.count("cancel") > 0).await;
    persisted(&manager, &fixture.0, 1).await;
    assert!(store.get_session("unblocked").unwrap().is_some());
    manager.shutdown().await;
    fixture.assert_reaped();
}

#[tokio::test]
async fn persistent_index_recovers_from_project_reconfiguration_and_marks_unsupported_coverage() {
    let fixture = Fixture::new("index-persistence-reconfigure");
    std::fs::create_dir_all(fixture.0.join(".cargo")).unwrap();
    std::fs::write(fixture.0.join("hang-symbols"), "").unwrap();
    let store = crate::store::Store::in_memory().unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    until(|| fixture.count("symbols") > 0).await;
    std::fs::remove_file(fixture.0.join("hang-symbols")).unwrap();
    // Retires a server with background work in flight; it must not stop the workspace index worker.
    std::fs::write(
        fixture.0.join(".cargo/config.toml"),
        "[net]\noffline=true\n",
    )
    .unwrap();
    until(|| manager.status(&fixture.0).unwrap().revision > 0).await;
    persisted(&manager, &fixture.0, 1).await;
    assert!(fixture.count("spawn") >= 2);
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .persistence
            .unwrap()
            .extracted_files,
        1
    );
    manager.shutdown().await;

    let mut config = fixture.config();
    config.servers.get_mut(&Language::Rust).unwrap().enabled = false;
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager.open_workspace(&fixture.0, config).unwrap();
    persisted(&manager, &fixture.0, 1).await;
    let coverage = manager.status(&fixture.0).unwrap().persistence.unwrap();
    assert_eq!(coverage.skipped_files, 1);
    let id = store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0].id;
    let records = store.index_files(id).unwrap();
    assert!(
        records[0].symbols.is_empty()
            && records[0]
                .symbols_state
                .contains("No available language server")
    );
    assert!(records[0].evidence_state.starts_with("unsupported:"));
    manager.shutdown().await;
}

/// Run explicitly against a disposable external project; never uses Oaklet as the sample.
#[tokio::test]
#[ignore = "requires OAKLET_INDEX_PROJECT pointing to a disposable Rust/TS project and real LSP tools"]
async fn real_project_sqlite_restore_probe() {
    let root =
        PathBuf::from(std::env::var_os("OAKLET_INDEX_PROJECT").expect("OAKLET_INDEX_PROJECT"))
            .canonicalize()
            .unwrap();
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    assert!(root.starts_with(&temporary) || root.starts_with("/private/tmp"));
    let database = temp_root("index-real-project-db");
    let path = database.join("oaklet.db");
    let mut files = 0;
    let mut extracted = 0;
    let mut snapshots = Vec::new();
    let mut query_files = Vec::new();
    for pass in 0..2 {
        let started = Instant::now();
        let store = crate::store::Store::open(&path).unwrap();
        let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
        manager
            .open_workspace(&root, CodeIndexConfig::default())
            .unwrap();
        let coverage = tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let state = manager.status(&root).unwrap();
                let coverage = state.persistence.unwrap();
                if let Some(error) = &coverage.error {
                    panic!("{error}");
                }
                if !state.discovering
                    && !state.stale
                    && !coverage.verifying
                    && !coverage.extracting
                    && !coverage.projects.is_empty()
                {
                    break coverage;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        eprintln!(
            "server_states={:?}",
            manager
                .status(&root)
                .unwrap()
                .projects
                .iter()
                .map(|p| (&p.language, &p.state))
                .collect::<Vec<_>>()
        );
        let coverage = coverage.expect("real project verification timed out");
        assert!(coverage.files > 0 && coverage.extracted_files > 0);
        for project in store.index_projects(&root.to_string_lossy()).unwrap() {
            let records = store.index_files(project.id).unwrap();
            if pass == 0 {
                let file = records
                    .iter()
                    .find(|f| f.extracted_hash.is_some())
                    .expect("each language needs a symbol query target");
                query_files.push(root.join(&file.path));
            }
            let count = records
                .iter()
                .filter(|f| f.extracted_hash.is_some())
                .count();
            let mut skipped = BTreeMap::new();
            for file in records.iter().filter(|f| f.extracted_hash.is_none()) {
                *skipped.entry(file.symbols_state.clone()).or_insert(0) += 1;
            }
            eprintln!(
                "language={:?} extracted={count} skipped={skipped:?}",
                project.language
            );
            assert!(
                count > 0,
                "real backend must extract symbols for each project language"
            );
        }
        let (_owner, cancel) = CancelHandle::new();
        for file in &query_files {
            let query = Query::Symbols {
                file: file.to_string_lossy().into_owned(),
            };
            let result = manager.query(&root, query.clone(), &cancel).await.unwrap();
            let expected = if pass == 0 {
                QuerySource::FileIndex
            } else {
                QuerySource::Sqlite
            };
            assert_eq!(result.source, expected);
            assert!(
                result.coverage.file_verified
                    && result.coverage.project_verified
                    && !result.completeness_known
            );
            assert_eq!(
                manager.query(&root, query, &cancel).await.unwrap().source,
                QuerySource::Memory
            );
            eprintln!(
                "pass={pass} symbol_query_source={:?} then=Memory file={}",
                result.source,
                file.display()
            );
        }
        manager.shutdown().await;
        assert!(!coverage.ipc_covered);
        if pass == 0 {
            files = coverage.files;
            extracted = coverage.extracted_files;
            snapshots = coverage
                .projects
                .iter()
                .map(|p| p.snapshot.clone())
                .collect();
        } else {
            assert_eq!(coverage.files, files);
            assert!(coverage.extracted_files >= extracted);
            assert_eq!(
                coverage.reused_files, extracted,
                "all previously extracted files must be reused without another request"
            );
            assert_eq!(
                coverage
                    .projects
                    .iter()
                    .map(|p| p.snapshot.clone())
                    .collect::<Vec<_>>(),
                snapshots
            );
        }
        eprintln!(
            "pass={pass} elapsed_ms={} files={} extracted={} reused={} skipped={} restored_projects={}",
            started.elapsed().as_millis(),
            coverage.files,
            coverage.extracted_files,
            coverage.reused_files,
            coverage.skipped_files,
            coverage.restored_projects
        );
    }
    std::fs::remove_dir_all(database).unwrap();
}

#[tokio::test]
async fn verified_symbols_hit_index_memory_sqlite_and_rebuild_only_the_current_workspace() {
    let a = Fixture::new("index-query-cache-a");
    let b = Fixture::new("index-query-cache-b");
    let database = temp_root("index-query-cache-db");
    let path = database.join("oaklet.db");
    let store = crate::store::Store::open(&path).unwrap();
    store
        .create_session("chat", &a.0.to_string_lossy(), 1)
        .unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    for f in [&a, &b] {
        manager.open_workspace(&f.0, f.config()).unwrap();
        persisted(&manager, &f.0, 1).await;
    }
    let (_owner, cancel) = CancelHandle::new();
    let first = manager.query(&a.0, symbols(), &cancel).await.unwrap();
    assert_eq!(first.source, QuerySource::FileIndex);
    assert!(first.coverage.file_verified && first.coverage.project_verified);
    assert!(!first.completeness_known && !first.coverage.semantic_ready);
    let absolute = Query::Symbols {
        file: a.0.join("src/./lib.rs").to_string_lossy().into_owned(),
    };
    assert_eq!(
        manager.query(&a.0, absolute, &cancel).await.unwrap().source,
        QuerySource::Memory
    );
    assert_eq!(a.count("symbols"), 1);
    manager.shutdown().await;
    drop(manager);
    drop(store);
    let store = crate::store::Store::open(&path).unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    for f in [&a, &b] {
        manager.open_workspace(&f.0, f.config()).unwrap();
        persisted(&manager, &f.0, 1).await;
    }
    assert_eq!(
        manager
            .query(&a.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::Sqlite
    );
    assert_eq!(
        manager
            .query(&a.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::Memory
    );
    assert_eq!(a.count("symbols"), 1);
    manager.close_workspace(&a.0);
    manager.drain().await;
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("UPDATE code_index_queries SET payload='invalid derived JSON' WHERE project_id IN (SELECT id FROM code_index_projects WHERE workspace=?1)", [a.0.to_string_lossy().as_ref()]).unwrap();
    manager.open_workspace(&a.0, a.config()).unwrap();
    persisted(&manager, &a.0, 1).await;
    assert_eq!(
        manager
            .query(&a.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::FileIndex
    );
    assert_eq!(
        a.count("symbols"),
        1,
        "a corrupt query row must not discard the valid file index"
    );
    let b_before = store.index_projects(&b.0.to_string_lossy()).unwrap()[0]
        .snapshot
        .clone();
    let bytes = std::fs::read(a.0.join("src/lib.rs")).unwrap();
    manager.rebuild(&a.0).await.unwrap();
    persisted(&manager, &a.0, 1).await;
    assert_eq!(a.count("symbols"), 2);
    assert_eq!(
        manager
            .query(&a.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::FileIndex
    );
    assert_eq!(
        store.index_projects(&b.0.to_string_lossy()).unwrap()[0].snapshot,
        b_before
    );
    assert_eq!(b.count("symbols"), 1);
    assert_eq!(std::fs::read(a.0.join("src/lib.rs")).unwrap(), bytes);
    assert!(store.get_session("chat").unwrap().is_some());
    manager.shutdown().await;
    a.assert_reaped();
    b.assert_reaped();
    std::fs::remove_dir_all(database).unwrap();
}

#[tokio::test]
async fn unavailable_backend_returns_only_byte_verified_symbols_without_semantic_cache() {
    let fixture = Fixture::new("index-query-offline");
    let store = crate::store::Store::in_memory().unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    manager.shutdown().await;
    let manager = Arc::new(CodeIndexManager::with_store(store));
    let mut config = fixture.config();
    config.servers.get_mut(&Language::Rust).unwrap().command =
        Some("/nonexistent-oaklet-language-server".into());
    manager.open_workspace(&fixture.0, config).unwrap();
    persisted(&manager, &fixture.0, 1).await;
    let (_owner, cancel) = CancelHandle::new();
    let result = manager.query(&fixture.0, symbols(), &cancel).await.unwrap();
    assert_eq!(result.source, QuerySource::FileIndex);
    assert!(result.coverage.file_verified);
    assert!(!result.coverage.project_verified && !result.coverage.semantic_ready);
    assert!(matches!(
        manager.query(&fixture.0, definition(0), &cancel).await,
        Err(IndexError::Backend(_))
    ));
    assert_eq!(manager.status(&fixture.0).unwrap().cache.entries, 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn cache_write_failure_preserves_live_results_and_semantic_queries_always_bypass_cache() {
    let fixture = Fixture::new("index-query-write-failure");
    let database = temp_root("index-query-write-failure-db");
    let path = database.join("oaklet.db");
    let store = crate::store::Store::open(&path).unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("query_write_failure.sql"))
        .unwrap();
    connection
        .execute("DELETE FROM code_index_symbols", [])
        .unwrap();
    connection
        .execute("UPDATE code_index_files SET extracted_hash=NULL", [])
        .unwrap();
    let (_owner, cancel) = CancelHandle::new();
    let result = manager.query(&fixture.0, symbols(), &cancel).await.unwrap();
    assert_eq!(result.source, QuerySource::Live);
    assert!(result.coverage.file_verified && !result.completeness_known);
    assert!(
        manager
            .status(&fixture.0)
            .unwrap()
            .cache
            .error
            .unwrap()
            .contains("injected query cache write failure")
    );
    assert_eq!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::Memory
    );
    std::fs::write(fixture.0.join("ready-definition"), "").unwrap();
    for _ in 0..2 {
        let result = manager
            .query(&fixture.0, definition(0), &cancel)
            .await
            .unwrap();
        assert_eq!(result.source, QuerySource::Live);
        assert!(!result.completeness_known && !result.coverage.semantic_ready);
    }
    assert_eq!(fixture.count("query"), 2);
    assert_eq!(manager.status(&fixture.0).unwrap().cache.entries, 1);
    manager.shutdown().await;
    std::fs::remove_dir_all(database).unwrap();
}

#[tokio::test]
async fn changed_contents_and_backend_versions_cannot_reuse_old_query_results() {
    let fixture = Fixture::new("index-query-invalidation");
    let store = crate::store::Store::in_memory().unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    let (_owner, cancel) = CancelHandle::new();
    assert_eq!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::FileIndex
    );
    assert_eq!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::Memory
    );
    let source = fixture.0.join("src/lib.rs");
    let metadata = std::fs::metadata(&source).unwrap();
    let original = std::fs::read_to_string(&source).unwrap();
    // Content is authoritative even before notify can deliver this equal-size/equal-mtime save.
    std::fs::write(&source, original.replace("alive", "other")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    let immediate = manager.query(&fixture.0, symbols(), &cancel).await;
    assert!(!matches!(
        immediate,
        Ok(QueryResult {
            source: QuerySource::Memory | QuerySource::Sqlite,
            ..
        })
    ));
    until(|| manager.status(&fixture.0).unwrap().revision > 0).await;
    persisted(&manager, &fixture.0, 1).await;
    assert_ne!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::Memory
    );
    manager.shutdown().await;
    let old = store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0]
        .fingerprint
        .clone();
    std::fs::write(fixture.0.join("backend-version"), "oaklet-fake-lsp 2").unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    assert_ne!(
        old,
        store.index_projects(&fixture.0.to_string_lossy()).unwrap()[0].fingerprint
    );
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .persistence
            .unwrap()
            .reused_files,
        0
    );
    assert_eq!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::FileIndex
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn empty_and_changed_during_query_responses_are_never_cached() {
    let fixture = Fixture::new("index-query-late");
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let (_owner, cancel) = CancelHandle::new();
    std::fs::write(fixture.0.join("empty-symbols"), "").unwrap();
    for _ in 0..2 {
        assert_eq!(
            manager
                .query(&fixture.0, symbols(), &cancel)
                .await
                .unwrap_err(),
            IndexError::NotReady
        );
    }
    assert_eq!(fixture.count("symbols"), 2);
    assert_eq!(manager.status(&fixture.0).unwrap().cache.entries, 0);
    std::fs::remove_file(fixture.0.join("empty-symbols")).unwrap();
    std::fs::write(fixture.0.join("slow-symbols"), "").unwrap();
    let pending = manager.query(&fixture.0, symbols(), &cancel);
    let change = async {
        until(|| fixture.count("symbols") == 3).await;
        std::fs::write(fixture.0.join("src/lib.rs"), "pub fn newer() {}\n").unwrap();
    };
    let (result, _) = tokio::join!(pending, change);
    assert_eq!(result.unwrap_err(), IndexError::NotReady);
    assert_eq!(manager.status(&fixture.0).unwrap().cache.entries, 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn rebuild_finishes_after_caller_drops_and_rejects_the_previous_generations_query() {
    use futures_util::FutureExt;
    let fixture = Fixture::new("index-query-rebuild-race");
    let store = crate::store::Store::in_memory().unwrap();
    let manager = Arc::new(CodeIndexManager::with_store(store.clone()));
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    persisted(&manager, &fixture.0, 1).await;
    let project = store
        .index_projects(&fixture.0.to_string_lossy())
        .unwrap()
        .remove(0);
    let mut file = store.index_files(project.id).unwrap().remove(0);
    file.extracted_hash = None;
    file.symbols.clear();
    store.index_replace_file(project.id, &file).unwrap();
    std::fs::write(fixture.0.join("slow-symbols"), "").unwrap();
    let (_owner, cancel) = CancelHandle::new();
    let pending = manager.query(&fixture.0, symbols(), &cancel);
    let rebuild = async {
        until(|| fixture.count("symbols") == 2).await;
        std::fs::remove_file(fixture.0.join("slow-symbols")).unwrap();
        // Poll once, then abandon the caller: the owned transition must still finish.
        assert!(manager.rebuild(&fixture.0).now_or_never().is_none());
        until(|| fixture.count("spawn") == 2).await;
        persisted(&manager, &fixture.0, 1).await;
    };
    let (result, _) = tokio::join!(pending, rebuild);
    assert_eq!(result.unwrap_err(), IndexError::Closed);
    assert_eq!(manager.status(&fixture.0).unwrap().cache.entries, 0);
    assert_eq!(
        manager
            .query(&fixture.0, symbols(), &cancel)
            .await
            .unwrap()
            .source,
        QuerySource::FileIndex
    );
    manager.shutdown().await;
    fixture.assert_reaped();
}

struct CodeToolAllow;
impl crate::tools::Approver for CodeToolAllow {
    fn approve<'a>(
        &'a self,
        _: crate::tools::ApprovalRequest,
    ) -> futures_util::future::BoxFuture<'a, crate::tools::ApprovalDecision> {
        Box::pin(async { crate::tools::ApprovalDecision::Allow })
    }
}
fn code_tools(manager: Arc<CodeIndexManager>) -> crate::tools::ToolRegistry {
    let mut registry = crate::tools::ToolRegistry::new();
    crate::tools::code::register(&mut registry, manager);
    registry
}
#[tokio::test]
async fn agent_code_tools_candidates_recursive_calls_and_spill_pages() {
    use serde_json::json;
    let fixture = Fixture::new("index-agent-tools");
    std::fs::write(fixture.0.join("src/lib.rs"), "a😀x\n").unwrap();
    std::fs::write(
        fixture.0.join("sync-options.json"),
        r#"{"semanticTools":true,"symbolCount":200}"#,
    )
    .unwrap();
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let registry = code_tools(manager.clone());
    let (mut ctx, root, _owner) = crate::tools::test_support::context("agent-code-context");
    ctx.workspace = crate::tools::Workspace::new(fixture.0.to_str().unwrap()).unwrap();
    let position = json!({"path":"src/lib.rs","line":1,"column":3});
    for name in ["code_definition", "code_references"] {
        let outcome = registry
            .execute(&ctx, &CodeToolAllow, name, name, position.clone())
            .await;
        assert!(outcome.ok, "{}", outcome.output);
        let value: Value = serde_json::from_str(&outcome.output).unwrap();
        assert_eq!(value["entries"][0]["column"], 3);
        assert_eq!(value["entries"][0]["path"], "src/lib.rs");
        assert_eq!(value["completeness_known"], false);
        if name == "code_definition" {
            assert_eq!(value["entries"][1]["external"], true);
            assert_eq!(value["entries"][1]["column_encoding"], "utf-16");
        }
    }
    assert_eq!(
        fixture
            .events()
            .iter()
            .find(|e| e["event"] == "position")
            .unwrap()["position"]["character"],
        3
    );
    for direction in ["incoming", "outgoing"] {
        let mut args = position.clone();
        args["direction"] = json!(direction);
        let outcome = registry
            .execute(&ctx, &CodeToolAllow, "code_calls", direction, args.clone())
            .await;
        assert!(outcome.ok, "{}", outcome.output);
        let value: Value = serde_json::from_str(&outcome.output).unwrap();
        assert_eq!(value["entries"].as_array().unwrap().len(), 2);
        assert_eq!(value["entries"][0]["name"], "same");
        assert_eq!(
            value["entries"][0]["subject"],
            value["entries"][0]["target"]
        );
        assert_eq!(value["truncated"], false);
        args["limit"] = json!(2);
        let limited = registry
            .execute(&ctx, &CodeToolAllow, "code_calls", "limited", args)
            .await;
        let value: Value = serde_json::from_str(&limited.output).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(value["entries"].as_array().unwrap().len(), 1);
    }
    for (limit, count, truncated) in [(None, 50, true), (Some(200), 200, false)] {
        let mut args = json!({"path":"src/lib.rs","query":"same"});
        if let Some(limit) = limit {
            args["limit"] = json!(limit);
        }
        let outcome = registry
            .execute(&ctx, &CodeToolAllow, "code_symbols", "search", args)
            .await;
        assert!(outcome.ok, "{}", outcome.output);
        let pointer = crate::tools::spill_pointer(&outcome.output).unwrap();
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(pointer).unwrap()).unwrap();
        assert_eq!(value["entries"].as_array().unwrap().len(), count);
        assert_eq!(value["truncated"], truncated);
        let paged = registry
            .execute(
                &ctx,
                &CodeToolAllow,
                "read_file",
                "page",
                json!({"path":pointer,"offset":3,"limit":2}),
            )
            .await;
        assert!(paged.ok, "{}", paged.output);
        assert!(paged.output.contains("file_verified"));
    }
    let invalid = registry
        .execute(
            &ctx,
            &CodeToolAllow,
            "code_symbols",
            "invalid",
            json!({"path":"../escape.rs"}),
        )
        .await;
    assert!(!invalid.ok);
    assert!(registry.warnings().is_empty());
    manager.shutdown().await;
    fixture.assert_reaped();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn agent_cancellation_and_missing_capabilities_keep_other_sessions_usable() {
    use serde_json::json;
    let fixture = Fixture::new("index-agent-cancel");
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&fixture.0, fixture.config())
        .unwrap();
    ready(&manager, &fixture.0).await;
    let registry = Arc::new(code_tools(manager.clone()));
    let (mut ctx, root, owner) = crate::tools::test_support::context("agent-cancel-context");
    ctx.workspace = crate::tools::Workspace::new(fixture.0.to_str().unwrap()).unwrap();
    let ctx = Arc::new(ctx);
    let pending = tokio::spawn({
        let registry = registry.clone();
        let ctx = ctx.clone();
        async move {
            registry
                .execute(
                    &ctx,
                    &CodeToolAllow,
                    "code_definition",
                    "cancel",
                    json!({"path":"src/lib.rs","line":1,"column":1}),
                )
                .await
        }
    });
    until(|| fixture.count("query") == 1).await;
    owner.cancel();
    assert!(!pending.await.unwrap().ok);
    until(|| fixture.count("cancel") == 1).await;
    let (mut other, other_root, _other_owner) =
        crate::tools::test_support::context("agent-other-context");
    other.workspace = ctx.workspace.clone();
    let outcome = registry
        .execute(
            &other,
            &CodeToolAllow,
            "code_calls",
            "unsupported",
            json!({"path":"src/lib.rs","line":1,"column":1,"direction":"incoming"}),
        )
        .await;
    assert!(
        !outcome.ok
            && outcome.output.contains("unsupported")
            && outcome.output.contains("grep/read_file")
    );
    let outcome = registry
        .execute(
            &other,
            &CodeToolAllow,
            "code_symbols",
            "survives",
            json!({"path":"src/lib.rs"}),
        )
        .await;
    assert!(outcome.ok, "{}", outcome.output);
    let outcome = registry
        .execute(
            &other,
            &CodeToolAllow,
            "grep",
            "fallback",
            json!({"pattern":"alive"}),
        )
        .await;
    assert!(outcome.ok, "{}", outcome.output);
    manager.close_workspace(&fixture.0);
    let outcome = registry
        .execute(
            &other,
            &CodeToolAllow,
            "code_symbols",
            "unavailable",
            json!({"path":"src/lib.rs"}),
        )
        .await;
    assert!(!outcome.ok && outcome.output.contains("unavailable"));
    manager.shutdown().await;
    fixture.assert_reaped();
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(other_root).unwrap();
}

#[tokio::test]
async fn call_prepare_candidates_are_bounded_and_report_truncation() {
    let fixture = Fixture::new("index-call-candidates");
    std::fs::write(
        fixture.0.join("sync-options.json"),
        r#"{"semanticTools":true,"prepareCount":34}"#,
    )
    .unwrap();
    let pool = LanguageServerManager::new();
    let key = fixture.register(&pool);
    let (_owner, cancel) = CancelHandle::new();
    let result = pool
        .query(
            &key,
            &fixture.0.join("src/lib.rs"),
            &Query::Calls {
                file: "src/lib.rs".into(),
                line: 0,
                column: 0,
                incoming: true,
            },
            &cancel,
        )
        .await
        .unwrap();
    assert!(result.truncated);
    let QueryData::Calls(items) = result.data else {
        panic!()
    };
    assert_eq!(items.len(), 32);
    assert_eq!(fixture.count("calls"), 32);
    for call in items {
        assert_eq!(call.subject, call.target);
    }
    pool.close_workspace(&fixture.0).await.unwrap();
    fixture.assert_reaped();
}

#[tokio::test]
#[ignore = "requires OAKLET_INDEX_PROJECT pointing to a disposable Acorn snapshot and real Rust/TS LSP tools"]
async fn real_project_agent_tools_probe() {
    use serde_json::json;
    let root =
        PathBuf::from(std::env::var_os("OAKLET_INDEX_PROJECT").expect("OAKLET_INDEX_PROJECT"))
            .canonicalize()
            .unwrap();
    assert!(
        root.starts_with(std::env::temp_dir().canonicalize().unwrap())
            || root.starts_with("/private/tmp")
    );
    let manager = Arc::new(CodeIndexManager::new());
    manager
        .open_workspace(&root, CodeIndexConfig::default())
        .unwrap();
    let registry = code_tools(manager.clone());
    let (mut ctx, context_root, _owner) = crate::tools::test_support::context("real-agent-tools");
    ctx.workspace = crate::tools::Workspace::new(root.to_str().unwrap()).unwrap();
    for (path, name) in [
        ("crates/acorn-core/src/agent/cost.rs", "format_cny"),
        ("frontend/src/lib/format.ts", "formatFieldValue"),
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        let (line, source) = text
            .lines()
            .enumerate()
            .find(|(_, line)| {
                line.contains(&format!("function {name}(")) || line.contains(&format!("fn {name}("))
            })
            .unwrap();
        let column = source[..source.find(name).unwrap()].chars().count() + 1;
        let position = json!({"path":path,"line":line+1,"column":column});
        let mut queries = vec![
            ("code_symbols", json!({"path":path})),
            ("code_symbols", json!({"path":path,"query":name})),
            ("code_definition", position.clone()),
            ("code_references", position.clone()),
            ("code_calls", {
                let mut args = position.clone();
                args["direction"] = json!("incoming");
                args
            }),
        ];
        if path.ends_with(".ts") {
            let mut args = position.clone();
            args["direction"] = json!("outgoing");
            queries.push(("code_calls", args));
        }
        for (tool, args) in queries {
            let deadline = Instant::now() + Duration::from_secs(90);
            let value = loop {
                let outcome = registry
                    .execute(&ctx, &CodeToolAllow, tool, tool, args.clone())
                    .await;
                if outcome.ok {
                    let full = crate::tools::spill_pointer(&outcome.output)
                        .map(|path| std::fs::read_to_string(path).unwrap())
                        .unwrap_or(outcome.output);
                    break serde_json::from_str::<Value>(&full).unwrap();
                }
                assert!(
                    Instant::now() < deadline,
                    "{tool} {path}: {}",
                    outcome.output
                );
                assert!(
                    outcome.output.contains("not_ready") || outcome.output.contains("unavailable"),
                    "{}",
                    outcome.output
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            assert_eq!(value["status"], "ok");
            assert_eq!(value["completeness_known"], false);
            assert!(!value["entries"].as_array().unwrap().is_empty());
            eprintln!(
                "tool={tool} path={path} entries={} source={} truncated={}",
                value["entries"].as_array().unwrap().len(),
                value["source"],
                value["truncated"]
            );
        }
    }
    manager.shutdown().await;
    std::fs::remove_dir_all(context_root).unwrap();
}

#[tokio::test]
async fn settings_update_all_workspaces_and_only_replace_affected_languages() {
    let a = Fixture::new("index-settings-a");
    let b = Fixture::new("index-settings-b");
    let manager = Arc::new(CodeIndexManager::new());
    let original = a.config();
    manager.open_workspace(&a.0, original.clone()).unwrap();
    manager.open_workspace(&b.0, original.clone()).unwrap();
    ready(&manager, &a.0).await;
    ready(&manager, &b.0).await;
    let status = manager.status(&a.0).unwrap();
    assert_eq!(
        status.projects[0].version.as_deref(),
        Some("oaklet-fake-lsp 1")
    );
    assert_eq!(status.revision, manager.status(&a.0).unwrap().revision);
    let mut config = original.clone();
    config.servers.insert(
        Language::TypeScript,
        ServerConfig {
            enabled: false,
            ..Default::default()
        },
    );
    manager.configure(config.clone()).unwrap();
    until(|| manager.status(&a.0).unwrap().revision > status.revision).await;
    ready(&manager, &a.0).await;
    ready(&manager, &b.0).await;
    assert_eq!(a.count("spawn"), 1);
    assert_eq!(b.count("spawn"), 1);
    assert_eq!(
        manager.status(&a.0).unwrap().projects[0].generation,
        status.projects[0].generation
    );
    config.servers.get_mut(&Language::Rust).unwrap().enabled = false;
    manager.configure(config.clone()).unwrap();
    until(|| {
        [&a.0, &b.0]
            .into_iter()
            .all(|root| manager.status(root).unwrap().projects[0].state == ServerState::Disabled)
    })
    .await;
    until(|| a.count("exit") == 1 && b.count("exit") == 1).await;
    assert!(manager.status(&a.0).unwrap().revision > status.revision);
    let mut invalid = config.clone();
    invalid.servers.get_mut(&Language::Rust).unwrap().command = Some("".into());
    assert!(matches!(
        manager.configure(invalid),
        Err(IndexError::Invalid(_))
    ));
    config.servers.get_mut(&Language::Rust).unwrap().enabled = true;
    config.servers.get_mut(&Language::Rust).unwrap().command =
        Some("/oaklet-missing-server".into());
    manager.configure(config).unwrap();
    until(|| {
        [&a.0, &b.0].into_iter().all(|root| {
            matches!(
                manager.status(root).unwrap().projects[0].state,
                ServerState::Failed(_)
            )
        })
    })
    .await;
    manager.configure(original.clone()).unwrap();
    ready(&manager, &a.0).await;
    ready(&manager, &b.0).await;
    assert_eq!(a.count("spawn"), 2);
    assert_eq!(b.count("spawn"), 2);
    // Later configuration-file notifications must use the new saved settings.
    std::fs::write(a.0.join("Cargo.lock"), "# updated lockfile\n").unwrap();
    until(|| a.count("spawn") >= 3).await;
    ready(&manager, &a.0).await;
    assert_eq!(b.count("spawn"), 2);
    manager.close_workspace(&a.0);
    manager.configure(original).unwrap();
    manager.drain().await;
    assert_eq!(manager.status(&a.0).unwrap_err(), IndexError::Closed);
    manager.open_workspace(&a.0, a.config()).unwrap();
    ready(&manager, &a.0).await;
    assert!(manager.status(&a.0).unwrap().revision > status.revision);
    let mut disabled = a.config();
    disabled.servers.get_mut(&Language::Rust).unwrap().enabled = false;
    manager.configure(disabled).unwrap();
    // Rebuild must preserve even a settings update still queued on the old worker.
    manager.rebuild(&a.0).await.unwrap();
    until(|| {
        [&a.0, &b.0].into_iter().all(|root| {
            manager
                .status(root)
                .unwrap()
                .projects
                .first()
                .is_some_and(|p| p.state == ServerState::Disabled)
        })
    })
    .await;
    manager.shutdown().await;
    #[cfg(unix)]
    {
        a.assert_reaped();
        b.assert_reaped();
    }
}

#[tokio::test]
async fn retry_selects_one_language_in_a_shared_project_root() {
    let fixture = Fixture::new("index-settings-mixed");
    std::fs::write(fixture.0.join("package.json"), "{}").unwrap();
    std::fs::write(fixture.0.join("src/index.ts"), "export const alive = 1;\n").unwrap();
    let manager = Arc::new(CodeIndexManager::new());
    let mut config = fixture.config();
    config.servers.insert(
        Language::TypeScript,
        ServerConfig {
            enabled: false,
            ..Default::default()
        },
    );
    manager.open_workspace(&fixture.0, config).unwrap();
    until(|| {
        manager.status(&fixture.0).is_ok_and(|status| {
            !status.stale
                && status.projects.len() == 2
                && status
                    .projects
                    .iter()
                    .any(|p| p.language == Language::Rust && p.state == ServerState::Ready)
        })
    })
    .await;
    let (_owner, cancel) = CancelHandle::new();
    manager
        .retry(&fixture.0, &fixture.0, Language::Rust, &cancel)
        .await
        .unwrap();
    assert!(
        manager
            .status(&fixture.0)
            .unwrap()
            .projects
            .iter()
            .any(|p| p.language == Language::TypeScript && p.state == ServerState::Disabled)
    );
    assert_eq!(
        manager
            .retry(&fixture.0, &fixture.0, Language::TypeScript, &cancel)
            .await
            .unwrap_err(),
        IndexError::Disabled
    );
    assert!(matches!(
        manager
            .retry(
                &fixture.0,
                &fixture.0.join("outside-project"),
                Language::Rust,
                &cancel
            )
            .await,
        Err(IndexError::Invalid(_))
    ));
    assert_eq!(fixture.count("spawn"), 2);
    std::fs::write(fixture.0.join("backend-version"), "oaklet-fake-lsp 2").unwrap();
    manager
        .retry(&fixture.0, &fixture.0, Language::Rust, &cancel)
        .await
        .unwrap();
    until(|| fixture.count("exit") == 2).await;
    assert_eq!(
        manager
            .status(&fixture.0)
            .unwrap()
            .projects
            .iter()
            .find(|p| p.language == Language::Rust)
            .unwrap()
            .version
            .as_deref(),
        Some("oaklet-fake-lsp 2")
    );
    manager.shutdown().await;
    #[cfg(unix)]
    fixture.assert_reaped();
}
