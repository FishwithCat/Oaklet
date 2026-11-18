//! Workspace-scoped code indexing service and managed language-server sessions.

pub(crate) mod commands;
mod config;
mod discovery;
pub(crate) mod documents;
mod framing;
mod language_servers;
mod lsp;
mod manager;
mod runtime;
mod types;
mod watch;

pub use crate::tools::{Cancel, CancelHandle};
pub use manager::CodeIndexManager;
pub use types::{
    CacheStatus, Call, IndexError, Location, PersistenceStatus, PersistentProjectStatus,
    ProjectStatus, Query, QueryCoverage, QueryData, QueryResult, QuerySource, ServerCapabilities,
    ServerState, Symbol, WorkspaceStatus,
};

pub use config::{CodeIndexConfig, ServerConfig};
pub use discovery::{Discovery, Project, discover};
pub use runtime::{ServerCommand, detect_server};

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
}

impl Language {
    const ALL: [Self; 5] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
    ];

    fn for_file(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod language_tests;
