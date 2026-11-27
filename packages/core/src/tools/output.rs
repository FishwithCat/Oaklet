use serde::Serialize;

use crate::spill;

use super::{FileDiff, ToolContext};

/// One image returned by a tool. `data_url` is a complete `data:` URL so the
/// agent loop can attach it to the next model request without another read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolImage {
    pub data_url: String,
}

/// How a shell command ended, or that it is still running in the background.
///
/// This is display metadata for the chat tool row. Older conversations stored
/// no outcome, so every consumer treats it as optional and falls back to the
/// tool's `ok` flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessOutcome {
    Succeeded,
    Exited {
        code: i32,
    },
    Signaled {
        signal: i32,
        #[serde(rename = "signalName")]
        signal_name: String,
    },
    TimedOut,
    Cancelled,
    Denied,
    Running {
        #[serde(rename = "jobId")]
        job_id: String,
    },
}

impl ProcessOutcome {
    /// Whether a finished process failed. A nonzero exit and a signal do;
    /// starting a background job does not.
    pub fn failed(&self) -> bool {
        match self {
            Self::Exited { code } => *code != 0,
            Self::Signaled { .. } => true,
            _ => false,
        }
    }

    /// Classify a finished foreground process from its real exit status.
    pub fn from_exit(status: std::process::ExitStatus) -> Self {
        if status.success() {
            return Self::Succeeded;
        }
        if let Some(code) = status.code() {
            return Self::Exited { code };
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let signal = status.signal().unwrap_or(0);
            Self::Signaled {
                signal,
                signal_name: signal_name(signal),
            }
        }
        #[cfg(not(unix))]
        {
            Self::Signaled {
                signal: 0,
                signal_name: "terminated".into(),
            }
        }
    }
}

/// A short name for the signals a local command commonly dies from.
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => return format!("signal {signal}"),
    };
    name.into()
}

/// Result of one tool call. Most tools return text only; a few attach images
/// the agent loop forwards as a separate user message, and file-mutation tools
/// attach display-only diffs the chat UI renders.
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub text: String,
    pub images: Vec<ToolImage>,
    pub diffs: Vec<FileDiff>,
    /// Set by tools that run a process; the chat row renders it.
    pub outcome: Option<ProcessOutcome>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            diffs: Vec::new(),
            outcome: None,
        }
    }

    pub fn with_image(text: impl Into<String>, data_url: String) -> Self {
        Self {
            text: text.into(),
            images: vec![ToolImage { data_url }],
            diffs: Vec::new(),
            outcome: None,
        }
    }

    pub fn with_diffs(text: impl Into<String>, diffs: Vec<FileDiff>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            diffs,
            outcome: None,
        }
    }

    pub fn with_outcome(mut self, outcome: ProcessOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl PartialEq<&str> for ToolOutput {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<str> for ToolOutput {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

/// Normalized result handed back to the agent loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
    /// Images returned by the call; the agent loop attaches them after the
    /// batch's tool messages so every provider sees a normal image user turn.
    pub images: Vec<ToolImage>,
    /// Applied diffs for the chat UI; empty for tools that do not mutate files.
    pub diffs: Vec<FileDiff>,
    /// Process outcome for command rows; absent for tools without a process.
    pub outcome: Option<ProcessOutcome>,
}

impl ToolOutcome {
    pub fn failure(message: impl Into<String>) -> Self {
        Self::failure_with_outcome(message, None)
    }

    pub fn failure_with_outcome(
        message: impl Into<String>,
        outcome: Option<ProcessOutcome>,
    ) -> Self {
        Self {
            ok: false,
            output: message.into(),
            images: Vec::new(),
            diffs: Vec::new(),
            outcome,
        }
    }
}

/// Tool output above this many characters keeps only its head and tail.
pub(super) const TOOL_OUTPUT_THRESHOLD_CHARS: usize = 12_000;
/// Head kept verbatim from an oversized tool output.
const TOOL_OUTPUT_HEAD_CHARS: usize = 4_000;
/// Tail kept verbatim: errors and summaries usually land at the end.
const TOOL_OUTPUT_TAIL_CHARS: usize = 1_500;
/// Marker substituted for the removed middle.
pub(super) const TOOL_OUTPUT_MARKER: &str = "\n\n[... tool output middle pruned ...]\n\n";
/// Reading a spill file must not spill again, or each page would be another file.
const TOOL_SPILL_SKIP: &str = "read_file";
/// The complete marker written when a tool output was spilled. Matching all of
/// it keeps ordinary log text that mentions `saved to:` from being mistaken for
/// a spill pointer.
const SPILL_MARKER_HEAD: &str = "[... tool output middle pruned; full output (";
const SPILL_MARKER_POINTER: &str = ") saved to: ";
const SPILL_MARKER_TAIL: &str = ". Use read_file with offset and limit to page through it.]";
const SPILL_RECOVERY_HEAD: &str =
    "[... tool output trimmed for context recovery; full output saved to: ";
/// Absolute spill locators are far shorter than this.
const SPILL_POINTER_MAX_CHARS: usize = 512;

/// Extract the path from a complete, Oaklet-generated spill marker.
pub(crate) fn spill_pointer(text: &str) -> Option<&str> {
    for head in [SPILL_MARKER_HEAD, SPILL_RECOVERY_HEAD] {
        for (start, _) in text.match_indices(head) {
            let rest = &text[start + head.len()..];
            let after_pointer = if head == SPILL_MARKER_HEAD {
                let Some((_, after_pointer)) = rest.split_once(SPILL_MARKER_POINTER) else {
                    continue;
                };
                after_pointer
            } else {
                rest
            };
            let Some((path, _)) = after_pointer.split_once(SPILL_MARKER_TAIL) else {
                continue;
            };
            if is_spill_path(path) && path.chars().count() <= SPILL_POINTER_MAX_CHARS {
                return Some(path);
            }
        }
    }
    None
}

/// Whether `path` looks like this app's absolute spill locator.
fn is_spill_path(path: &str) -> bool {
    path.contains("/spill/session-") || path.contains("\\spill\\session-")
}

/// Save oversized output before trimming it, so the full text survives in
/// a file the model can page through. A failed write keeps the old marker.
pub(super) fn bound_output(ctx: &ToolContext, tool: &str, call_id: &str, text: String) -> String {
    if text.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS {
        return text;
    }
    let spilled = (tool != TOOL_SPILL_SKIP)
        .then(|| {
            spill::save(
                &ctx.spill_root,
                &ctx.session_id,
                &format!("{tool}-{call_id}"),
                &text,
            )
        })
        .flatten();
    let marker = match &spilled {
        Some(spilled) => format!(
            "\n\n{SPILL_MARKER_HEAD}{} lines, {} bytes{SPILL_MARKER_POINTER}{}{SPILL_MARKER_TAIL}\n\n",
            spilled.lines,
            spilled.bytes,
            spilled.path.display()
        ),
        None => TOOL_OUTPUT_MARKER.to_string(),
    };
    prune_tool_output(text, &marker)
}

pub(crate) fn cap(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut capped: String = text.chars().take(max_chars).collect();
    capped.push_str("\n... [truncated]");
    capped
}

/// Keep the head and tail of an oversized tool output.
///
/// The middle of a file dump or command log is the least useful part, while the
/// head carries the shape of the result and the tail carries errors and
/// summaries. Slicing is by character, so multi-byte text cannot be split. The
/// marker is budgeted too, so a long spill path cannot grow the result past the
/// threshold.
fn prune_tool_output(text: String, marker: &str) -> String {
    let total = text.chars().count();
    if total <= TOOL_OUTPUT_THRESHOLD_CHARS {
        return text;
    }
    let budget = TOOL_OUTPUT_THRESHOLD_CHARS.saturating_sub(marker.chars().count());
    let head_end = TOOL_OUTPUT_HEAD_CHARS.min(total).min(budget);
    let tail_count = TOOL_OUTPUT_TAIL_CHARS
        .min(total - head_end)
        .min(budget - head_end);
    let mut pruned: String = text.chars().take(head_end).collect();
    pruned.push_str(marker);
    pruned.extend(text.chars().skip(total - tail_count));
    pruned
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn caps_long_output_at_char_boundaries() {
        let text = "日本語".repeat(10);
        let capped = cap(text, 5);
        assert!(capped.starts_with("日本語日本"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn keeps_short_tool_output_verbatim() {
        let text = "x".repeat(TOOL_OUTPUT_THRESHOLD_CHARS);
        assert_eq!(prune_tool_output(text.clone(), TOOL_OUTPUT_MARKER), text);
    }

    #[test]
    fn prunes_the_middle_of_a_long_tool_output() {
        let head = "h".repeat(TOOL_OUTPUT_HEAD_CHARS);
        let tail = "t".repeat(TOOL_OUTPUT_TAIL_CHARS);
        let text = format!("{head}{}{tail}", "m".repeat(TOOL_OUTPUT_THRESHOLD_CHARS));
        let pruned = prune_tool_output(text, TOOL_OUTPUT_MARKER);

        assert!(pruned.starts_with(&head));
        assert!(pruned.ends_with(&tail));
        assert_eq!(pruned.matches(TOOL_OUTPUT_MARKER).count(), 1);
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn prunes_multibyte_output_without_splitting_characters() {
        let text = "日本語".repeat(10_000);
        let pruned = prune_tool_output(text, TOOL_OUTPUT_MARKER);

        assert!(pruned.starts_with(&"日本語".repeat(TOOL_OUTPUT_HEAD_CHARS / 3)));
        assert!(pruned.ends_with(&"日本語".repeat(TOOL_OUTPUT_TAIL_CHARS / 3)));
        assert!(!pruned.contains('\u{fffd}'));
        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
    }

    #[test]
    fn a_long_marker_stays_inside_the_pruning_budget() {
        let text = "x".repeat(TOOL_OUTPUT_THRESHOLD_CHARS * 2);
        let marker = format!("[saved to {}]", "p".repeat(TOOL_OUTPUT_THRESHOLD_CHARS / 2));
        let pruned = prune_tool_output(text, &marker);

        assert!(pruned.chars().count() <= TOOL_OUTPUT_THRESHOLD_CHARS);
        assert!(pruned.contains(&marker));
    }

    #[test]
    fn spill_pointer_ignores_saved_to_text_before_the_marker() {
        let path = "/Users/me/Library/Application Support/com.oaklet.desktop/spill/session-0123456789abcdef/out.txt";
        let text = format!(
            "artifact saved to: build/report.html\n\n{SPILL_MARKER_HEAD}900 lines, 20000 bytes{SPILL_MARKER_POINTER}{path}{SPILL_MARKER_TAIL}"
        );

        assert_eq!(spill_pointer(&text), Some(path));

        let legacy = format!(
            "{SPILL_MARKER_HEAD}1 lines, 2 bytes{SPILL_MARKER_POINTER}.oaklet/spill/run-1/out.txt{SPILL_MARKER_TAIL}"
        );
        assert_eq!(spill_pointer(&legacy), None);

        let not_a_spill = format!(
            "{SPILL_MARKER_HEAD}1 lines, 2 bytes{SPILL_MARKER_POINTER}build/report.html{SPILL_MARKER_TAIL}"
        );
        assert_eq!(spill_pointer(&not_a_spill), None);
    }

    #[test]
    fn pruning_budget_fits_inside_the_threshold() {
        assert!(
            TOOL_OUTPUT_HEAD_CHARS + TOOL_OUTPUT_MARKER.chars().count() + TOOL_OUTPUT_TAIL_CHARS
                <= TOOL_OUTPUT_THRESHOLD_CHARS
        );
    }

    #[test]
    fn process_outcomes_serialize_in_the_desktop_shape() {
        assert_eq!(
            json!(ProcessOutcome::Exited { code: 2 }),
            json!({ "kind": "exited", "code": 2 })
        );
        assert_eq!(
            json!(ProcessOutcome::Signaled {
                signal: 9,
                signal_name: "SIGKILL".into(),
            }),
            json!({ "kind": "signaled", "signal": 9, "signalName": "SIGKILL" })
        );
        assert_eq!(
            json!(ProcessOutcome::Running {
                job_id: "job-1".into(),
            }),
            json!({ "kind": "running", "jobId": "job-1" })
        );
    }
}
