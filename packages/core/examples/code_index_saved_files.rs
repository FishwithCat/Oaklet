//! Mutating acceptance probe for a disposable Acorn snapshot under the OS temp directory.
use oaklet_lib::code_index::{
    CancelHandle, CodeIndexConfig, CodeIndexManager, IndexError, Query, QueryData, ServerState,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Restore(Vec<(PathBuf, Option<String>)>);
impl Restore {
    fn track(&mut self, path: PathBuf) -> Result<()> {
        if !self.0.iter().any(|(old, _)| old == &path) {
            let saved = match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            self.0.push((path, saved));
        }
        Ok(())
    }
}
impl Drop for Restore {
    fn drop(&mut self) {
        for (path, text) in &self.0 {
            if let Some(text) = text {
                let _ = std::fs::write(path, text);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
async fn refs(
    manager: &CodeIndexManager,
    root: &Path,
    file: &str,
    symbol: &str,
) -> Result<Vec<oaklet_lib::code_index::Location>> {
    let text = std::fs::read_to_string(root.join(file))?;
    let (line, source) = text
        .lines()
        .enumerate()
        .find(|(_, line)| {
            line.contains(&format!("fn {symbol}(")) || line.contains(&format!("function {symbol}("))
        })
        .ok_or("Symbol declaration missing")?;
    let column = source[..source.find(symbol).unwrap()]
        .encode_utf16()
        .count() as u32;
    let (_owner, cancel) = CancelHandle::new();
    match manager
        .query(
            root,
            Query::References {
                file: file.into(),
                line: line as u32,
                column,
            },
            &cancel,
        )
        .await?
    {
        oaklet_lib::code_index::QueryResult {
            data: QueryData::Locations(items),
            ..
        } => Ok(items),
        _ => Err("Wrong result type".into()),
    }
}
async fn expect(
    manager: &CodeIndexManager,
    root: &Path,
    file: &str,
    symbol: &str,
    caller: &Path,
    line: u32,
    present: bool,
) -> Result<Value> {
    let started = Instant::now();
    let mut attempts = 0;
    loop {
        attempts += 1;
        match refs(manager, root, file, symbol).await {
            Ok(items)
                if items
                    .iter()
                    .any(|item| item.path == caller && item.line == line)
                    == present =>
            {
                return Ok(
                    json!({"elapsedMs": started.elapsed().as_millis(), "attempts": attempts, "referenceCount": items.len(), "complete": false}),
                );
            }
            Ok(_) | Err(_) if started.elapsed() < Duration::from_secs(120) => {}
            Err(error) => return Err(error),
            _ => return Err("Saved reference expectation timed out".into()),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn probe(
    manager: &CodeIndexManager,
    root: &Path,
    restore: &mut Restore,
    file: &str,
    symbol: &str,
    caller: &str,
    call: &str,
) -> Result<Value> {
    let caller = root.join(caller);
    restore.track(caller.clone())?;
    let original = std::fs::read_to_string(&caller)?;
    let line = original.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let baseline = expect(manager, root, file, symbol, &caller, line, false).await?;
    let (_owner, cancel) = CancelHandle::new();
    manager
        .query(
            root,
            Query::Symbols {
                file: caller.to_string_lossy().into_owned(),
            },
            &cancel,
        )
        .await?;
    // This in-memory string is deliberately not written yet.
    let edited = format!("{original}\n{call}\n");
    let unsaved = expect(manager, root, file, symbol, &caller, line, false).await?;
    let pending = caller.with_extension("oaklet-pending");
    restore.track(pending.clone())?;
    std::fs::write(&pending, edited)?;
    std::fs::rename(&pending, &caller)?;
    let saved = expect(manager, root, file, symbol, &caller, line, true).await?;
    // Move the entire caller file; check both the old location and the new one.
    let moved = caller.with_file_name(format!(
        "oaklet_moved.{}",
        caller.extension().unwrap().to_string_lossy()
    ));
    if moved.exists() {
        return Err("Probe destination already exists".into());
    }
    restore.track(moved.clone())?;
    std::fs::rename(&caller, &moved)?;
    let renamed = expect(manager, root, file, symbol, &caller, line, false).await?;
    let module = root.join("crates/acorn-core/src/agent/mod.rs");
    let mut original_module = None;
    if caller.extension().is_some_and(|ext| ext == "rs") {
        restore.track(module.clone())?;
        let text = std::fs::read_to_string(&module)?;
        std::fs::write(
            &module,
            text.replace(
                "mod dispatch;",
                "#[path = \"oaklet_moved.rs\"] mod dispatch;",
            ),
        )?;
        original_module = Some(text);
    }
    let moved_seen = expect(manager, root, file, symbol, &moved, line, true).await?;
    std::fs::remove_file(&moved)?;
    let deleted = expect(manager, root, file, symbol, &moved, line, false).await?;
    std::fs::write(&caller, original)?;
    if let Some(text) = original_module {
        std::fs::write(&module, text)?;
    }
    Ok(
        json!({"baseline": baseline, "unsaved": unsaved, "saved": saved, "oldPathAfterRename": renamed, "newPathAfterRename": moved_seen, "deleted": deleted}),
    )
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arg = std::env::args()
        .nth(1)
        .ok_or("Usage: code_index_saved_files DISPOSABLE_ACORN_SNAPSHOT")?;
    let root = Path::new(&arg).canonicalize()?;
    let temp = std::env::temp_dir().canonicalize()?;
    if !root.starts_with(&temp) && !root.starts_with("/private/tmp") {
        return Err(
            "Only a disposable snapshot inside the OS temp directory may be mutated".into(),
        );
    }
    let manager = Arc::new(CodeIndexManager::new());
    let mut restore = Restore(Vec::new());
    let work = async {
        manager.open_workspace(&root, CodeIndexConfig::default())?;
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let status = manager.status(&root)?;
                if !status.discovering
                    && !status.stale
                    && status
                        .projects
                        .iter()
                        .all(|p| p.state == ServerState::Ready)
                {
                    return Ok::<_, IndexError>(());
                }
                if status
                    .projects
                    .iter()
                    .any(|p| matches!(p.state, ServerState::Failed(_)))
                {
                    return Err(IndexError::Backend(format!("{status:?}")));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        let rust = probe(
            &manager,
            &root,
            &mut restore,
            "crates/acorn-core/src/agent/cost.rs",
            "format_cny",
            "crates/acorn-core/src/agent/dispatch.rs",
            "fn oaklet_saved_probe() { let _ = super::cost::format_cny(1.0); }",
        )
        .await?;
        let typescript = probe(
            &manager,
            &root,
            &mut restore,
            "frontend/src/lib/format.ts",
            "formatFieldValue",
            "frontend/src/lib/format.test.ts",
            "function oaklet_saved_probe() { return formatFieldValue('string', '😀'); }",
        )
        .await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"rust":rust,"typescript":typescript}))?
        );
        Ok(())
    }
    .await;
    manager.shutdown().await;
    drop(restore);
    work
}
