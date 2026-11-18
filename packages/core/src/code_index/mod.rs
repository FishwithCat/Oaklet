//! Project discovery and runtime detection for workspace-scoped code indexing.

mod config;
mod discovery;
mod runtime;

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
}

impl Language {
    fn for_file(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
