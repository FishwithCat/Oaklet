//! Values persisted by the event log and handed to the UI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::projection;
use crate::agent::Usage;

/// One append-only transcript entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// A model-visible message. `display` carries UI-only data and never
    /// reaches a model request.
    Message {
        message: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<Value>,
    },
    /// A model-only replacement; the original message remains available for recall.
    ToolTrim { original_seq: i64, message: Value },
    /// Exact model projection; original message bodies remain in the log.
    Context {
        surface: Vec<projection::SurfaceEntry>,
        /// Durable record of this replacement's archived originals; the recall
        /// index in archived_messages is written in the same transaction.
        archived: Vec<i64>,
        before: usize,
        after: usize,
    },
    /// User-visible text that is not part of the conversation.
    Notice { tone: String, text: String },
    /// A summary replacing the transcript prefix through `shadowed_through`.
    Compaction {
        summary: Value,
        shadowed_through: i64,
        before: usize,
        after: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub payload: EventPayload,
    pub created_at: i64,
}

/// Token gauge persisted with a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: usize,
    pub window: usize,
}

/// Provider-reported tokens accumulated over one run. Steps counts successful
/// model requests, including those whose provider omitted usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cache_hit_tokens: usize,
    pub cache_miss_tokens: usize,
    pub steps: usize,
    /// Counts reported by the newest step, kept for the restored composer gauge.
    pub last_prompt_tokens: usize,
    pub last_completion_tokens: usize,
    pub last_cache_hit_tokens: usize,
    pub last_cache_miss_tokens: usize,
}

impl RunUsage {
    pub fn add(&mut self, usage: Option<Usage>) {
        self.steps += 1;
        if let Some(usage) = usage {
            self.prompt_tokens += usage.prompt_tokens;
            self.completion_tokens += usage.completion_tokens;
            self.cache_hit_tokens += usage.cache_hit_tokens;
            self.cache_miss_tokens += usage.cache_miss_tokens;
            self.last_prompt_tokens = usage.prompt_tokens;
            self.last_completion_tokens = usage.completion_tokens;
            self.last_cache_hit_tokens = usage.cache_hit_tokens;
            self.last_cache_miss_tokens = usage.cache_miss_tokens;
        }
    }
}

/// Provider-reported usage for one model request, restored with a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cache_hit_tokens: usize,
    pub cache_miss_tokens: usize,
}

/// A stored conversation: metadata plus its append-only event log.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
    pub events: Vec<Event>,
}

/// A session's metadata without its event log.
#[derive(Clone, Debug)]
pub struct SessionMeta {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
}

/// One contiguous slice of a session log, newest window or run delta.
#[derive(Clone, Debug)]
pub struct SessionWindow {
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub turns: usize,
    pub updated_at: i64,
    pub context: Option<ContextUsage>,
    /// Newest model request in this session, or `None` before the first reply.
    pub last_usage: Option<StepUsage>,
    pub events: Vec<Event>,
    /// Whether events exist before the first event in this window.
    pub has_more: bool,
}

/// A started run: the new UI window, its first event, and the model history
/// that preceded it. The history is read inside the append transaction.
#[derive(Clone, Debug)]
pub struct RunStart {
    pub window: SessionWindow,
    pub start_seq: i64,
    pub prior_messages: Vec<Value>,
}

/// A lightweight row for the history palette.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub turns: usize,
}

/// The user turn being appended before a run starts.
#[derive(Clone, Debug)]
pub struct NewUserTurn {
    /// Model-facing message, with no display-only fields.
    pub message: Value,
    /// UI-only metadata such as attachment thumbnails and the text to show.
    pub display: Option<Value>,
    /// Set only when this is the first user message of the session.
    pub title: Option<String>,
}

/// Everything a finished run appends in one transaction.
#[derive(Clone, Debug)]
pub struct RunEnd {
    pub session_id: String,
    pub run_id: String,
    /// Seq of the user event appended by `begin_run`.
    pub start_seq: i64,
    pub status: &'static str,
    pub finished_at: i64,
    pub appended: Vec<EventPayload>,
    /// Present when this run compacted its history.
    pub compaction: Option<CompactionEnd>,
    pub context: Option<ContextUsage>,
    pub usage: RunUsage,
    pub error: Option<String>,
}

/// Exact context projection to append after this run's messages.
#[derive(Clone, Debug)]
pub struct CompactionEnd {
    /// Model history with stable identities; no content matching is needed.
    pub surface: Vec<Value>,
    pub before: usize,
    pub after: usize,
}

/// One compaction write: summary, boundary, and the gauge it produced.
#[derive(Clone, Debug)]
#[cfg(test)]
pub struct CompactionRecord {
    pub summary: Value,
    pub shadowed_through: i64,
    pub before: usize,
    pub after: usize,
    pub context: ContextUsage,
}
