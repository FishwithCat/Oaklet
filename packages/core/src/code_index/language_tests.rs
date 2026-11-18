//! Opt-in semantic checks against locally installed backends, without installing dependencies.
use super::*;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn probe(
    language: Language,
    declaration: &str,
    caller: &str,
    marker: &str,
    source: &str,
    edited: &str,
    manifest: Option<(&str, &str)>,
) {
    let fixture = Fixture(crate::tools::test_support::temp_root("real-language-index"));
    std::fs::create_dir_all(&fixture.0).unwrap();
    let helper = match language {
        Language::Python => "def greet():\n    return 1\n",
        Language::Go => "package fixture\n\nfunc Greet() int { return 1 }\n",
        Language::Java => "public class Helper {\n    public static int greet() { return 1; }\n}\n",
        _ => unreachable!(),
    };
    std::fs::write(fixture.0.join(declaration), helper).unwrap();
    std::fs::write(fixture.0.join(caller), source).unwrap();
    if let Some((name, text)) = manifest {
        std::fs::write(fixture.0.join(name), text).unwrap();
    }
    let manager = Arc::new(CodeIndexManager::new());
    let (_owner, cancel) = CancelHandle::new();
    let work = async {
        manager
            .open_workspace(&fixture.0, CodeIndexConfig::default())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        let status = loop {
            let status = manager.status(&fixture.0).unwrap();
            if let Some(project) = status.projects.first() {
                assert!(
                    !matches!(project.state, ServerState::Failed(_)),
                    "{status:?}"
                );
                if !status.stale && project.state == ServerState::Ready {
                    break status;
                }
            }
            assert!(
                Instant::now() < deadline,
                "Backend startup timed out: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        eprintln!("{}", serde_json::to_string(&status).unwrap());
        let position = |text: &str| {
            let (line, source) = text
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(marker) && !line.starts_with("from "))
                .unwrap();
            (
                line as u32,
                source[..source.find(marker).unwrap()]
                    .encode_utf16()
                    .count() as u32,
            )
        };
        let (line, column) = position(source);
        let definition = Query::Definition {
            file: caller.into(),
            line,
            column,
        };
        let (line, column) = position(helper);
        let references = Query::References {
            file: declaration.into(),
            line,
            column,
        };
        let mut previous_count = 0;
        for (query, target, update) in [
            (
                Query::Symbols {
                    file: declaration.into(),
                },
                declaration,
                false,
            ),
            (definition, declaration, false),
            (references.clone(), caller, false),
            (references, caller, true),
        ] {
            if update {
                std::fs::write(fixture.0.join(caller), edited).unwrap();
            }
            let minimum = if update { previous_count + 1 } else { 1 };
            let started = Instant::now();
            loop {
                match manager.query(&fixture.0, query.clone(), &cancel).await {
                    Ok(QueryResult {
                        data: QueryData::Symbols(items),
                        ..
                    }) if !items.is_empty() => break,
                    Ok(QueryResult {
                        data: QueryData::Locations(items),
                        ..
                    }) if items
                        .iter()
                        .filter(|item| item.path == fixture.0.join(target))
                        .count()
                        >= minimum =>
                    {
                        previous_count = items
                            .iter()
                            .filter(|item| item.path == fixture.0.join(target))
                            .count();
                        break;
                    }
                    Ok(_) | Err(IndexError::NotReady)
                        if started.elapsed() < Duration::from_secs(60) => {}
                    result => panic!("Expected {minimum} locations in {target}: {result:?}"),
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            eprintln!(
                "{language:?} {query:?}: {} ms",
                started.elapsed().as_millis()
            );
        }
        let (line, column) = position(helper);
        for incoming in [true, false] {
            let result = manager
                .query(
                    &fixture.0,
                    Query::Calls {
                        file: declaration.into(),
                        line,
                        column,
                        incoming,
                    },
                    &cancel,
                )
                .await;
            if status.projects[0].capabilities.as_ref().unwrap().calls {
                assert!(matches!(result.unwrap().data, QueryData::Calls(_)));
            } else {
                assert!(matches!(result, Err(IndexError::Unsupported(_))));
            }
        }
    };
    // Always stop child processes, including when an assertion fails.
    use futures_util::FutureExt;
    let result = std::panic::AssertUnwindSafe(work).catch_unwind().await;
    manager.shutdown().await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
#[ignore = "requires locally installed Pyright, Node.js and Python"]
async fn real_pyright_semantics_and_saved_updates() {
    probe(
        Language::Python,
        "helper.py",
        "main.py",
        "greet",
        "from helper import greet\n\ndef run():\n    return greet()\n",
        "from helper import greet\n\ndef run():\n    return greet() + greet()\n",
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires locally installed gopls and Go"]
async fn real_gopls_semantics_and_saved_updates() {
    probe(
        Language::Go,
        "helper.go",
        "main.go",
        "Greet",
        "package fixture\n\nfunc Run() int { return Greet() }\n",
        "package fixture\n\nfunc Run() int { return Greet() + Greet() }\n",
        Some(("go.mod", "module example.test/fixture\n\ngo 1.20\n")),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires locally installed JDT LS, JDK 21+ and Python 3"]
async fn real_jdtls_semantics_and_saved_updates() {
    probe(
        Language::Java,
        "Helper.java",
        "Main.java",
        "greet",
        "public class Main { static int run() { return Helper.greet(); } }\n",
        "public class Main { static int run() { return Helper.greet() + Helper.greet(); } }\n",
        None,
    )
    .await;
}
