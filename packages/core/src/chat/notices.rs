//! Deliver settled background jobs to busy or idle sessions.
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use super::{
    ChatState,
    run::{RunContext, RunTrigger, start_run},
};
use crate::SharedState;
use crate::store::NewUserTurn;
use crate::tools::CancelHandle;

/// Largest job output a completion notice carries inline; a bigger one leaves
/// the read to `job_output`.
const INLINE_NOTICE_CHARS: usize = 2_000;
/// Turns one session may open from job notices between two user messages, so a
/// job whose completion starts another job cannot wake it forever.
const MAX_WAKES: u32 = 8;
/// Longest command label shown in a notice summary.
const NOTICE_LABEL_CHARS: usize = 80;

/// A settled background job waiting for its session: the model-facing message
/// it carries.
#[derive(Clone)]
pub struct JobNotice {
    pub message: Value,
}

/// Announce a settled background job to the session that started it. A busy
/// session gets the notice queued for its next step; an idle one wakes with it,
/// bounded by [`MAX_WAKES`] so a job whose completion starts another job cannot
/// wake the session forever.
pub(crate) fn job_settled(
    app: &AppHandle,
    state: &SharedState,
    job: crate::tools::jobs::JobSettled,
) {
    let notice = JobNotice {
        message: job_notice(&job),
    };
    let session_id = job.owner.session_id.clone();
    let window = job.owner.window.clone();
    let chat = &state.chat;
    // A plan under review must not be bypassed by a job completion.
    let pending = chat
        .store
        .plan_state(&session_id)
        .map(|plan| plan.pending)
        .unwrap_or(true);

    let wake = {
        let mut live = chat.live();
        let busy = pending
            || live.runs.values().any(|run| run.session_id == session_id)
            || live.compacting.contains_key(&session_id)
            || live.closing.contains(&window)
            || live
                .unsaved
                .values()
                .any(|(_, end)| end.session_id == session_id);
        let spent = live.wakes.get(&session_id).copied().unwrap_or(0);
        if busy || spent >= MAX_WAKES {
            live.notices
                .entry(session_id.clone())
                .or_default()
                .push(notice.clone());
            false
        } else {
            true
        }
    };
    if !wake {
        return;
    }

    if let Err(error) = wake_notice(app, state, &window, &session_id, notice.clone()) {
        crate::log::key(format_args!("[oaklet] job notice not delivered: {error}"));
        queue_notice(chat, &session_id, notice);
    }
}

/// Open one turn on an idle session so it reads a settled job's result.
fn wake_notice(
    app: &AppHandle,
    state: &SharedState,
    window: &str,
    session_id: &str,
    notice: JobNotice,
) -> Result<(), String> {
    if app.get_webview_window(window).is_none() {
        return Err("The window is closed.".into());
    }
    let Some(meta) = state.chat.store.session_meta(session_id)? else {
        return Err("The session no longer exists.".into());
    };
    let mut config = state
        .config
        .lock()
        .map_err(|_| "Agent state is unavailable.".to_string())?
        .clone();
    config.workspace = meta.workspace;
    let turn = NewUserTurn {
        message: notice.message,
        display: Some(json!({ "hidden": true })),
        title: None,
    };
    let (cancel_handle, cancel) = CancelHandle::new();
    start_run(
        app,
        state,
        window,
        session_id,
        RunTrigger::Notice,
        config,
        turn,
        cancel,
        cancel_handle,
    )?;
    let spent = state
        .chat
        .live()
        .wakes
        .get(session_id)
        .copied()
        .unwrap_or(0);
    state
        .chat
        .live()
        .wakes
        .insert(session_id.to_string(), spent.saturating_add(1));
    Ok(())
}

fn queue_notice(chat: &ChatState, session_id: &str, notice: JobNotice) {
    chat.live()
        .notices
        .entry(session_id.to_string())
        .or_default()
        .push(notice);
}

/// Deliver notices that settled while the session was mid-run but arrived
/// after its last step: the run's end wakes the session once with them.
pub(super) fn resume_notices(ctx: &RunContext) {
    let chat = &ctx.state().chat;
    if chat
        .store
        .plan_state(ctx.session_id())
        .map(|plan| plan.pending)
        .unwrap_or(true)
    {
        return;
    }
    let spent = {
        let live = chat.live();
        if live.notices.get(ctx.session_id()).is_none_or(Vec::is_empty) {
            return;
        }
        live.wakes.get(ctx.session_id()).copied().unwrap_or(0)
    };
    if spent >= MAX_WAKES {
        return;
    }
    let notices = chat.take_notices(ctx.session_id());
    let text = notices
        .iter()
        .filter_map(|notice| {
            notice
                .message
                .pointer("/content/0/text")
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let notice = JobNotice {
        message: json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
    };
    if let Err(error) = wake_notice(
        ctx.app(),
        ctx.state(),
        ctx.window_label(),
        ctx.session_id(),
        notice.clone(),
    ) {
        crate::log::key(format_args!("[oaklet] job notice not delivered: {error}"));
        queue_notice(chat, ctx.session_id(), notice);
    }
}

/// The model-facing completion text.
fn job_notice(job: &crate::tools::jobs::JobSettled) -> Value {
    let line = job.command.lines().next().unwrap_or("").trim();
    let mut label: String = line.chars().take(NOTICE_LABEL_CHARS).collect();
    if label.chars().count() < line.chars().count() {
        label.push('…');
    }
    let origin = if label.is_empty() {
        String::new()
    } else {
        format!(" (bash: {label})")
    };
    let status = match job.exit {
        Some(code) => format!("[status: {}, exit code: {code}]", job.status),
        None => format!("[status: {}]", job.status),
    };
    let summary = format!("background job {}{origin} finished {status}", job.id);
    let body = job.output.trim_end();
    let text = if job.output.chars().count() <= INLINE_NOTICE_CHARS {
        if body.is_empty() {
            format!("{summary}. No new output.")
        } else {
            format!("{summary}. Output:\n{body}")
        }
    } else {
        format!("{summary}. Read its output with job_output.")
    };
    json!({ "role": "user", "content": [{ "type": "text", "text": text }] })
}

#[cfg(test)]
mod tests {
    use super::{ChatState, JobNotice, job_notice};
    use crate::tools::jobs::{JobOwner, JobSettled};
    use serde_json::json;

    fn settled(output: &str) -> JobSettled {
        JobSettled {
            id: "job-1a1ec825407-2".into(),
            owner: JobOwner {
                session_id: "s1".into(),
                window: "main".into(),
            },
            command: "cargo test".into(),
            status: "succeeded",
            exit: Some(0),
            output: output.into(),
        }
    }

    #[test]
    fn small_output_rides_along_and_large_output_points_at_job_output() {
        let message = job_notice(&settled("ok\n"));
        let text = message["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("background job job-1a1ec825407-2"));
        assert!(text.contains("ok"));

        let message = job_notice(&settled(&"x".repeat(super::INLINE_NOTICE_CHARS + 1)));
        let text = message["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("job_output"));
        assert!(!text.contains("xxx"));
    }

    #[test]
    fn queued_notices_are_taken_once() {
        let state = ChatState::in_memory();
        state
            .live()
            .notices
            .entry("s1".into())
            .or_default()
            .push(JobNotice { message: json!({}) });
        assert_eq!(state.take_notices("s1").len(), 1);
        assert!(state.take_notices("s1").is_empty());
    }
}
