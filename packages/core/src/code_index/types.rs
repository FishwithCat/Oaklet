use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
pub enum Query {
    Symbols {
        file: String,
    },
    WorkspaceSymbols {
        file: String,
        query: String,
    },
    Definition {
        file: String,
        line: u32,
        column: u32,
    },
    References {
        file: String,
        line: u32,
        column: u32,
    },
    Calls {
        file: String,
        line: u32,
        column: u32,
        incoming: bool,
    },
}

impl Query {
    pub(super) fn file(&self) -> &str {
        match self {
            Self::Symbols { file }
            | Self::WorkspaceSymbols { file, .. }
            | Self::Definition { file, .. }
            | Self::References { file, .. }
            | Self::Calls { file, .. } => file,
        }
    }
}

/// Positions use zero-based lines and UTF-16 columns; the LSP adapter converts negotiated encodings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: u32,
    pub location: Location,
    pub children: Vec<Symbol>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub subject: Location,
    pub name: String,
    pub target: Location,
    pub sites: Vec<Location>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum QueryData {
    Symbols(Vec<Symbol>),
    Locations(Vec<Location>),
    Calls(Vec<Call>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    #[serde(skip)]
    pub(crate) content_hash: Option<String>,
    pub source: QuerySource,
    pub coverage: QueryCoverage,
    pub data: QueryData,
    pub generation: u64,
    pub truncated: bool,
    /// Protocol initialization cannot establish semantic completeness.
    pub completeness_known: bool,
}

/// A cached symbol list can have verified bytes without complete semantic coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySource {
    Live,
    Memory,
    Sqlite,
    FileIndex,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QueryCoverage {
    pub file_verified: bool,
    pub project_verified: bool,
    pub semantic_ready: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CacheStatus {
    pub entries: usize,
    pub bytes: usize,
    pub error: Option<String>,
    pub semantic_cache_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum IndexError {
    Busy,
    Closed,
    NotReady,
    Disabled,
    Cancelled,
    Timeout,
    Unsupported(String),
    Invalid(String),
    Backend(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for IndexError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServerState {
    Idle,
    Queued,
    Starting,
    /// Protocol initialized; semantic completeness remains unknown.
    Ready,
    Failed(String),
    Disabled,
    Closed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerCapabilities {
    pub symbols: bool,
    pub workspace_symbols: bool,
    pub definition: bool,
    pub references: bool,
    pub calls: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectStatus {
    pub root: PathBuf,
    pub language: super::Language,
    pub state: ServerState,
    pub capabilities: Option<ServerCapabilities>,
    pub progress: Vec<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceStatus {
    pub cache: CacheStatus,
    pub persistence: Option<PersistenceStatus>,
    pub revision: u64,
    pub stale: bool,
    pub discovering: bool,
    pub incomplete: bool,
    pub warnings: Vec<String>,
    pub projects: Vec<ProjectStatus>,
}

/// Verification is about disk metadata; it does not establish LSP semantic completeness.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PersistenceStatus {
    pub verifying: bool,
    pub extracting: bool,
    pub restored_projects: usize,
    pub total_files: usize,
    pub files: usize,
    pub extracted_files: usize,
    pub reused_files: usize,
    pub skipped_files: usize,
    pub ipc_covered: bool,
    pub error: Option<String>,
    pub projects: Vec<PersistentProjectStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PersistentProjectStatus {
    pub root: PathBuf,
    pub language: super::Language,
    pub snapshot: String,
    pub verified: bool,
}
