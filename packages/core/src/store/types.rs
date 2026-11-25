//! Values persisted by the event log and handed to the UI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::projection;
use crate::agent::Usage;
use crate::architecture::Diagnostic;
use crate::context::{self, ContextSegment};

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
    /// An implementation plan waiting for the user's decision. Only the newest
    /// version is actionable; a later version supersedes earlier ones.
    Plan {
        version: u64,
        status: PlanStatus,
        /// The assistant's final plan text, kept with the card so a restored or
        /// compacted session still shows the implementation steps.
        #[serde(default)]
        steps: String,
        /// Structured review content the agent submitted with `present_plan`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Box<PlanDetails>>,
        /// Backend validation of the plan's architecture draft, when the agent
        /// used `validate_architecture_diff`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validation: Option<Box<PlanValidation>>,
    },
}

/// Lifecycle of a persisted plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Waiting for the user to confirm or revise it.
    Pending,
    /// The user confirmed this version and a run consumed it.
    Accepted,
}

/// Outcome of validating a plan's LikeC4 draft against the selected model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanValidation {
    pub status: PlanValidationStatus,
    pub valid: bool,
    #[serde(default)]
    pub model_root: String,
    #[serde(default)]
    pub include_ignored: bool,
    #[serde(default)]
    pub source_digest: String,
    #[serde(default)]
    pub diff_id: Option<String>,
    #[serde(default)]
    pub dsl: String,
    /// The upstream diff summary, kept as JSON so the card can render counts
    /// and affected views without another helper run.
    #[serde(default)]
    pub diff: Option<Value>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub warnings: Vec<Diagnostic>,
    #[serde(default)]
    pub overlaps: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The reviewable plan the agent presents before confirmation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDetails {
    /// What the user asked for and why it needs this change.
    pub objective: String,
    /// The code and model changes, in natural language.
    pub changes: String,
    pub steps: Vec<PlanStep>,
    /// Risks that could change the implementation decision.
    #[serde(default)]
    pub risks: Vec<String>,
    /// Questions the user should settle before implementation starts.
    #[serde(default)]
    pub questions: Vec<String>,
}

/// One ordered implementation step, with the evidence needed to review it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub title: String,
    /// What is true when this step is finished.
    pub outcome: String,
    /// Workspace-relative files this step touches.
    #[serde(default)]
    pub files: Vec<String>,
    /// Architecture element or relation IDs this step implements.
    #[serde(default)]
    pub components: Vec<String>,
    /// One-based numbers of steps that must finish first.
    #[serde(default)]
    pub depends_on: Vec<u64>,
    pub verification: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanValidationStatus {
    #[default]
    Required,
    Verified,
    Invalid,
    Unavailable,
}

/// One persisted plan version, with everything confirmation needs.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanRecord {
    pub version: u64,
    pub status: PlanStatus,
    pub steps: String,
    pub details: Option<PlanDetails>,
    pub validation: Option<PlanValidation>,
}

impl PlanRecord {
    pub fn validation_for_confirmation(&self) -> Result<Option<&PlanValidation>, String> {
        match &self.validation {
            None => Ok(None),
            Some(validation) => match validation.status {
                PlanValidationStatus::Verified if validation.valid => Ok(Some(validation)),
                PlanValidationStatus::Unavailable => Ok(None),
                _ => Err("This plan's architecture draft has not been verified. Revise the plan before implementing it.".into()),
            },
        }
    }
}

/// The newest plan version, and whether it still needs review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanState {
    pub latest: Option<u64>,
    pub pending: bool,
    pub requires_validation: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub payload: EventPayload,
    pub created_at: i64,
}

/// Token gauge persisted with a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: usize,
    pub window: usize,
    pub segments: Vec<ContextSegment>,
}

impl ContextUsage {
    pub fn measured(used: usize, window: usize, segments: Vec<ContextSegment>) -> Self {
        Self {
            used,
            window,
            segments: context::scaled(segments, used),
        }
    }

    pub fn segments_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.segments).map_err(|error| error.to_string())
    }
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
    /// Plan state read in the same transaction that appended the user turn.
    pub plan: PlanState,
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
