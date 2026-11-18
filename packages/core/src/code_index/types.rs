use std::path::PathBuf;

use serde::Serialize;

#[derive(Clone, Debug)]
pub enum Query {
    Symbols {
        file: String,
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
            | Self::Definition { file, .. }
            | Self::References { file, .. }
            | Self::Calls { file, .. } => file,
        }
    }
}

/// Positions use zero-based lines and UTF-16 columns, matching negotiated LSP encoding.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: u32,
    pub location: Location,
    pub children: Vec<Symbol>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Call {
    pub name: String,
    pub target: Location,
    pub sites: Vec<Location>,
}

#[derive(Clone, Debug, Serialize)]
pub enum QueryData {
    Symbols(Vec<Symbol>),
    Locations(Vec<Location>),
    Calls(Vec<Call>),
}

#[derive(Clone, Debug, Serialize)]
pub struct QueryResult {
    pub data: QueryData,
    pub generation: u64,
    pub truncated: bool,
    /// Protocol initialization cannot establish semantic completeness.
    pub completeness_known: bool,
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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
    pub discovering: bool,
    pub incomplete: bool,
    pub warnings: Vec<String>,
    pub projects: Vec<ProjectStatus>,
}
