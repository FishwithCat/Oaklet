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
        .retry(&fixture.0, &fixture.0, &cancel)
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
        .retry(&fixture.0, &fixture.0, &cancel)
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
    until(|| fixture.count("files") >= 1).await;
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
