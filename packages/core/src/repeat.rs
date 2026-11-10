//! Advisory guard against a model repeating one identical tool call.
//!
//! The guard never blocks a call. At 3, 5, and 8 identical calls in a row it
//! returns a model-facing reminder that the next request carries as a user
//! message, so a stuck model can change approach or finish.

use serde_json::Value;

/// Consecutive identical calls that earn a reminder.
const THRESHOLDS: [usize; 3] = [3, 5, 8];
/// Arguments shown in the detailed reminder; the full value is always compared.
const PREVIEW_CHARS: usize = 500;
/// Marks an advisory message as injected rather than typed by the user.
/// Compaction uses this to keep the latest human request out of a summary.
pub(crate) const REMINDER_PREFIX: &str = "Oaklet loop guard: ";

/// True when a message was injected by this guard.
pub(crate) fn is_reminder(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with(REMINDER_PREFIX))
}

/// Tracks one run's consecutive identical calls.
#[derive(Default)]
pub struct RepeatGuard {
    last: Option<(String, String)>,
    count: usize,
}

impl RepeatGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one call and return a reminder when its count is a threshold.
    pub fn observe(&mut self, name: &str, args: &Value) -> Option<String> {
        let key = (name.to_string(), canonical(args));
        if self.last.as_ref() == Some(&key) {
            self.count += 1;
        } else {
            self.last = Some(key);
            self.count = 1;
        }

        if !THRESHOLDS.contains(&self.count) {
            return None;
        }

        let count = self.count;
        if count == THRESHOLDS[0] {
            return Some(format!(
                "{REMINDER_PREFIX}you have called `{name}` {count} times in a row with identical \
arguments. The repeated calls added no new information. Inspect the previous result before \
calling it again: change approach, gather different evidence, or finish the task."
            ));
        }

        Some(format!(
            "{REMINDER_PREFIX}`{name}` has now been called {count} times in a row with identical \
arguments. Calling it again will not help. Stop, review the earlier results, and either try a \
different approach or finish the task.\nRepeated arguments (truncated): {}",
            preview(args)
        ))
    }
}

/// Render JSON with object keys sorted, so property order cannot disguise a repeat.
fn canonical(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    write(map.get(key).unwrap_or(&Value::Null), out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }

    let mut out = String::new();
    write(value, &mut out);
    out
}

fn preview(args: &Value) -> String {
    let text = args.to_string();
    if text.chars().count() <= PREVIEW_CHARS {
        return text;
    }
    let mut short: String = text.chars().take(PREVIEW_CHARS).collect();
    short.push_str("...");
    short
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RepeatGuard, is_reminder};

    #[test]
    fn stays_quiet_until_the_third_identical_call() {
        let mut guard = RepeatGuard::new();
        let args = json!({ "path": "src/main.rs" });

        assert!(guard.observe("read_file", &args).is_none());
        assert!(guard.observe("read_file", &args).is_none());
        let reminder = guard.observe("read_file", &args).expect("third call");
        assert!(reminder.contains("3 times"));
        assert!(reminder.contains("read_file"));
        assert!(reminder.starts_with(super::REMINDER_PREFIX));
    }

    #[test]
    fn the_detailed_reminder_quotes_the_arguments() {
        let mut guard = RepeatGuard::new();
        let args = json!({ "command": "cargo test" });

        assert!(guard.observe("run_command", &args).is_none());
        assert!(guard.observe("run_command", &args).is_none());
        assert!(guard.observe("run_command", &args).is_some());
        assert!(guard.observe("run_command", &args).is_none());
        let reminder = guard.observe("run_command", &args).expect("fifth call");
        assert!(reminder.contains("5 times"));
        assert!(reminder.contains("cargo test"));
    }

    #[test]
    fn a_different_call_resets_the_chain() {
        let mut guard = RepeatGuard::new();
        let first = json!({ "path": "src/a.rs" });
        let second = json!({ "path": "src/b.rs" });

        assert!(guard.observe("read_file", &first).is_none());
        assert!(guard.observe("read_file", &first).is_none());
        assert!(guard.observe("read_file", &second).is_none());
        assert!(guard.observe("read_file", &second).is_none());
        assert!(guard.observe("read_file", &second).is_some());
        assert!(guard.observe("list_dir", &second).is_none());
    }

    #[test]
    fn argument_key_order_does_not_break_the_chain() {
        let mut guard = RepeatGuard::new();
        let first = json!({ "a": 1, "b": 2 });
        let reordered = json!({ "b": 2, "a": 1 });

        assert!(guard.observe("write_file", &first).is_none());
        assert!(guard.observe("write_file", &reordered).is_none());
        assert!(guard.observe("write_file", &first).is_some());
    }

    #[test]
    fn only_guard_messages_count_as_reminders() {
        assert!(is_reminder(
            &json!({ "role": "user", "content": "Oaklet loop guard: change approach" })
        ));
        assert!(!is_reminder(
            &json!({ "role": "user", "content": "fix the tests" })
        ));
        assert!(!is_reminder(
            &json!({ "role": "assistant", "content": "Oaklet loop guard: x" })
        ));
    }
}
