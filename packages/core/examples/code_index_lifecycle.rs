//! Exercise the production workspace service without Tauri UI or an Agent.
use std::{path::Path, sync::Arc, time::Duration};

use oaklet_lib::code_index::{CodeIndexConfig, CodeIndexManager, ServerState, WorkspaceStatus};
use serde_json::json;

async fn settled(manager: &CodeIndexManager, root: &Path) -> Result<WorkspaceStatus, String> {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let status = manager.status(root).map_err(|error| error.to_string())?;
            if !status.discovering
                && !status.stale
                && status.projects.iter().all(|project| {
                    !matches!(project.state, ServerState::Queued | ServerState::Starting)
                })
            {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "Workspace initialization timed out".to_string())?
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .ok_or("Usage: code_index_lifecycle WORKSPACE")?;
    let root = Path::new(&root).canonicalize()?;
    let manager = Arc::new(CodeIndexManager::new());
    let work = async {
        manager.open_workspace(&root, CodeIndexConfig::default())?;
        let first = settled(&manager, &root).await?;
        manager.open_workspace(&root, CodeIndexConfig::default())?;
        let duplicate = manager.status(&root)?;
        manager.close_workspace(&root);
        manager.drain().await;
        manager.open_workspace(&root, CodeIndexConfig::default())?;
        let reopened = settled(&manager, &root).await?;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"first": first, "duplicate": duplicate, "reopened": reopened})
            )?
        );
        if reopened
            .projects
            .iter()
            .any(|project| !matches!(project.state, ServerState::Ready | ServerState::Disabled))
        {
            return Err("A language server did not initialize".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    manager.shutdown().await;
    work
}
