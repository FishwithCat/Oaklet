//! Discovers model-facing tools from configured MCP servers and caches their connections.
//!
//! A failed server becomes a prompt warning and never stops the run.

mod client;
mod tool;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use self::client::{McpClient, ToolSpec};
use self::tool::McpTool;
use super::{Cancel, Tool};
use crate::McpServerConfig;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// MCP servers stay connected between runs. The cap bounds child processes and
/// cached tool schemas when the user has configured many servers.
const MAX_CACHED_SERVERS: usize = 32;

/// Connect to every enabled server in parallel and turn its tools into tools the
/// agent can call. Errors are reported to the model instead of ending the run.
pub async fn discover(
    servers: &BTreeMap<String, McpServerConfig>,
    workspace: &Path,
    cancel: &Cancel,
) -> (Vec<Box<dyn Tool>>, Vec<String>) {
    let enabled: Vec<(String, McpServerConfig, String)> = servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, server)| {
            (
                name.clone(),
                server.clone(),
                cache_key(name, server, workspace),
            )
        })
        .collect();

    let mut entries = tokio::select! {
        biased;
        _ = cancel.cancelled() => return (Vec::new(), Vec::new()),
        entries = cache().lock() => entries,
    };
    // A server that was disabled or reconfigured must not keep its old child
    // process or HTTP session alive. Other workspaces keep their own entries.
    let prefix = format!("{}\u{0}", workspace.display());
    let current: HashSet<&str> = enabled.iter().map(|(_, _, key)| key.as_str()).collect();
    entries.retain(|key, _| !key.starts_with(&prefix) || current.contains(key.as_str()));

    let mut outcomes: Vec<Option<Result<CachedServer, String>>> = Vec::with_capacity(enabled.len());
    let mut misses = Vec::new();
    for (index, (name, server, key)) in enabled.iter().enumerate() {
        if let Some(entry) = entries.get_mut(key) {
            entry.last_used = Instant::now();
            outcomes.push(Some(Ok(entry.server.clone())));
        } else {
            outcomes.push(None);
            misses.push((index, name.clone(), server.clone(), key.clone()));
        }
    }

    let tasks = misses.into_iter().map(|(index, name, server, key)| {
        let workspace = workspace.to_path_buf();
        async move {
            let result = tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                discover_server(&name, &server, &workspace),
            )
            .await;
            (index, name, key, result)
        }
    });

    let results = tokio::select! {
        biased;
        _ = cancel.cancelled() => return (Vec::new(), Vec::new()),
        results = futures_util::future::join_all(tasks) => results,
    };
    for (index, name, key, result) in results {
        let outcome = match result {
            Ok(Ok((client, specs))) => {
                let cached = CachedServer {
                    client: Arc::new(Mutex::new(Some(client))),
                    specs: Arc::new(specs),
                };
                entries.insert(
                    key,
                    CachedEntry {
                        server: cached.clone(),
                        last_used: Instant::now(),
                    },
                );
                Ok(cached)
            }
            Ok(Err(error)) => Err(format!("MCP server \"{name}\" failed: {error}")),
            Err(_) => Err(format!("MCP server \"{name}\" timed out during startup.")),
        };
        outcomes[index] = Some(outcome);
    }
    prune_cache(&mut entries);
    drop(entries);

    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    let mut warnings = Vec::new();
    let mut used_names = HashSet::new();

    for ((server_name, server, _), outcome) in enabled.into_iter().zip(outcomes) {
        let Some(result) = outcome else { continue };
        match result {
            Ok(cached) => {
                for spec in cached.specs.iter() {
                    tools.push(Box::new(McpTool::new(
                        &server_name,
                        &server,
                        spec,
                        cached.client.clone(),
                        &mut used_names,
                    )));
                }
            }
            Err(error) => warnings.push(error),
        }
    }

    (tools, warnings)
}

/// Keep the most recently used servers when the cache is over its bound.
fn prune_cache(entries: &mut HashMap<String, CachedEntry>) {
    if entries.len() <= MAX_CACHED_SERVERS {
        return;
    }
    let mut by_age: Vec<(Instant, String)> = entries
        .iter()
        .map(|(key, entry)| (entry.last_used, key.clone()))
        .collect();
    let excess = by_age.len() - MAX_CACHED_SERVERS;
    by_age.sort_by_key(|(last_used, _)| *last_used);
    for (_, key) in by_age.drain(..excess) {
        entries.remove(&key);
    }
}

async fn discover_server(
    name: &str,
    server: &McpServerConfig,
    workspace: &Path,
) -> Result<(McpClient, Vec<ToolSpec>), String> {
    let mut client = McpClient::connect(name, server, workspace).await?;
    let specs = client.list_tools().await?;
    Ok((client, specs))
}

/// A live client plus the schemas it advertised. Shared by every run that
/// uses the same server config, so a chat message does not restart stdio
/// servers or lose the HTTP session.
#[derive(Clone)]
struct CachedServer {
    client: Arc<Mutex<Option<McpClient>>>,
    specs: Arc<Vec<ToolSpec>>,
}

struct CachedEntry {
    server: CachedServer,
    last_used: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<String, CachedEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CachedEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop every cached client so `kill_on_drop` terminates stdio servers before
/// the app process exits. Statics are not dropped during normal shutdown.
pub(crate) fn shutdown() {
    if let Some(cache) = CACHE.get()
        && let Ok(mut entries) = cache.try_lock()
    {
        entries.clear();
    }
}

/// Release the servers owned by one workspace when its window closes.
pub(crate) fn evict_workspace(workspace: &Path) {
    let workspace = workspace.to_path_buf();
    tauri::async_runtime::spawn(async move {
        let mut entries = cache().lock().await;
        let prefix = format!("{}\u{0}", workspace.display());
        entries.retain(|key, _| !key.starts_with(&prefix));
    });
}

/// Server name and config are part of the key: editing a command, URL, or
/// header must start a new connection instead of reusing the old one.
fn cache_key(name: &str, server: &McpServerConfig, workspace: &Path) -> String {
    let config = serde_json::to_string(server).unwrap_or_default();
    format!("{}\u{0}{name}\u{0}{config}", workspace.display())
}
