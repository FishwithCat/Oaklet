//! One model request, including the single context-overflow retry.
//!
//! A provider can reject a prompt that was under the local trigger; the only
//! recovery is to compact harder and retry once before reporting the error.

use serde_json::Value;

use super::ToolCall;
use super::ensure_history_tool;
use super::model::stream_step;
use super::window::note_compaction;
use crate::AppConfig;
use crate::chat::RunContext;
use crate::compaction;
use crate::context::estimate_tokens;
use crate::tools::{Cancel, ToolRegistry};

/// Total attempts for a transient provider failure, including the first.
const MAX_ATTEMPTS: usize = 3;

/// The assistant turn the run should act on.
pub(super) struct Reply {
    pub content: String,
    pub reasoning: String,
    pub calls: Vec<ToolCall>,
    pub prompt_tokens: Option<usize>,
}

pub(super) enum Next {
    Reply(Reply),
    /// History was force-compacted; send the smaller request right away.
    Retry,
}

pub(super) async fn request(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    cancel: &Cancel,
    ctx: &RunContext,
    overflow_retried: &mut bool,
) -> Result<Next, String> {
    let mut attempt = 0;
    loop {
        match stream_step(ctx, client, config, tools, messages, cancel).await {
            Ok((content, reasoning, calls, prompt_tokens)) => {
                *overflow_retried = false;
                return Ok(Next::Reply(Reply {
                    content,
                    reasoning,
                    calls,
                    prompt_tokens,
                }));
            }
            Err(err) if err.is_transient() && attempt + 1 < MAX_ATTEMPTS => {
                let delay = err.retry_delay(attempt);
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = cancel.cancelled() => return Err("Stopped.".into()),
                }
            }
            Err(err) if err.is_context_overflow() && !*overflow_retried => {
                *overflow_retried = true;
                match compaction::force(client, config, messages, tools, cancel).await {
                    Ok(Some(compacted)) => {
                        let before = estimate_tokens(messages)
                            + crate::context::estimate_text(&tools.schemas().to_string());
                        ctx.mark_compacted(before);
                        *messages = compacted.messages;
                        ctx.record_archived(messages).await?;
                        ensure_history_tool(tools, ctx).await;
                        let after = estimate_tokens(messages)
                            + crate::context::estimate_text(&tools.schemas().to_string());
                        note_compaction(before, after);
                        if after > config.context_window {
                            return Err("The current request and tool schemas still exceed the context window.".into());
                        }
                        return Ok(Next::Retry);
                    }
                    Ok(None) => {
                        return Err(format!(
                            "{err}\n\nThe conversation could not be compacted further; \
                             start a new chat or raise the context window."
                        ));
                    }
                    Err(force_err) => return Err(force_err),
                }
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::agent::messages::assistant_message;
    use crate::compaction::test_support::summary_server_with;
    use crate::store::{EventPayload, NewUserTurn, RunEnd, Store};
    use crate::tools::{CancelHandle, test_support::temp_root};

    #[tokio::test]
    async fn overflow_retry_persists_compaction_for_the_next_run() {
        let (base_url, seen, server) = summary_server_with(vec![
            (
                400,
                r#"{"error":{"message":"maximum context length exceeded","code":"context_length_exceeded"}}"#,
            ),
            (
                200,
                r#"{"choices":[{"message":{"content":"Goal: finish the task. Earlier work is complete."}}]}"#,
            ),
            (
                200,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Completed.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            ),
        ]);
        let root = temp_root("overflow-persistence");
        let database = root.join("oaklet.db");
        let config = AppConfig {
            base_url,
            workspace: root.to_string_lossy().into_owned(),
            context_window: 262_144,
            ..AppConfig::default()
        };
        let store = Store::open(&database).unwrap();
        store.create_session("s1", &config.workspace, 1).unwrap();
        let old_request = json!({"role": "user", "content": "old detail ".repeat(2000)});
        let first = store
            .begin_run(
                "s1",
                "r1",
                &NewUserTurn {
                    message: old_request.clone(),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        store
            .finish_run(&RunEnd {
                session_id: "s1".into(),
                run_id: "r1".into(),
                start_seq: first.start_seq,
                status: "done",
                finished_at: 3,
                appended: vec![EventPayload::Message {
                    message: json!({"role": "assistant", "content": "Earlier work is complete."}),
                    display: None,
                }],
                compaction: None,
                context: None,
                error: None,
            })
            .unwrap();
        let state = Arc::new(crate::AppState {
            exit: Mutex::new(crate::shutdown::ExitState::Running),
            config: Mutex::new(config.clone()),
            config_path: root.join("config.json"),
            config_write: Mutex::new(()),
            windows: Mutex::new(crate::WindowRegistry::default()),
            spill_root: root.join("spill"),
            approvals: Mutex::new(Default::default()),
            chat: crate::chat::ChatState::open(&database).unwrap(),
            command_allow_all: Mutex::new(Default::default()),
            terminal: Mutex::new(Default::default()),
            jobs: Arc::new(crate::tools::jobs::JobRegistry::new()),
            architecture: Mutex::new(Default::default()),
            ids: std::sync::atomic::AtomicU64::new(0),
        });
        let current = json!({"role": "user", "content": "Continue with the task."});
        let started = store
            .begin_run(
                "s1",
                "r2",
                &NewUserTurn {
                    message: current.clone(),
                    display: None,
                    title: None,
                },
                4,
            )
            .unwrap();
        let mut messages = vec![json!({"role": "system", "content": "You are Oaklet."})];
        messages.extend(started.prior_messages.clone());
        messages.push(current);
        let before = estimate_tokens(&messages)
            + crate::context::estimate_text(&ToolRegistry::new().schemas().to_string());
        assert!(before < compaction::trigger_at(config.context_window));
        let ctx = RunContext::without_window(state, "s1", "r2", started);
        let (_handle, cancel) = CancelHandle::new();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mut tools = ToolRegistry::new();
        let mut overflow_retried = false;

        assert!(matches!(
            request(
                &client,
                &config,
                &mut tools,
                &mut messages,
                &cancel,
                &ctx,
                &mut overflow_retried
            )
            .await
            .unwrap(),
            Next::Retry
        ));
        assert!(overflow_retried);
        assert!(tools.get("history_read").is_some());
        assert!(crate::store::is_checkpoint(&messages[1]));
        let Next::Reply(reply) = request(
            &client,
            &config,
            &mut tools,
            &mut messages,
            &cancel,
            &ctx,
            &mut overflow_retried,
        )
        .await
        .unwrap() else {
            panic!("compacted request did not complete")
        };
        assert!(!overflow_retried);
        assert_eq!(reply.content, "Completed.");
        let assistant = assistant_message(&reply.content, &reply.reasoning, &reply.calls);
        ctx.record_message(assistant.clone(), Some(json!({"runId": "r2"})));
        messages.push(assistant);
        let expected = messages[1..].to_vec();
        ctx.finish_done(expected.clone()).await;
        drop(ctx);
        drop(tools);
        drop(store);
        server.join().unwrap();

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let requests: Vec<Value> = requests
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        assert_eq!(requests[0]["stream"], true);
        assert_eq!(requests[1]["stream"], false);
        assert_eq!(requests[2]["stream"], true);
        assert_eq!(requests[0]["messages"][1], old_request);
        assert_eq!(
            requests[2]["messages"],
            json!(crate::context::request_messages(
                &messages[..messages.len() - 1]
            ))
        );

        let reopened = Store::open(&database).unwrap();
        let next = reopened
            .begin_run(
                "s1",
                "r3",
                &NewUserTurn {
                    message: json!({"role": "user", "content": "Next task."}),
                    display: None,
                    title: None,
                },
                6,
            )
            .unwrap();
        assert_eq!(next.prior_messages, expected);
        let saved = reopened.get_session("s1").unwrap().unwrap();
        assert!(saved.events.iter().any(|event| matches!(&event.payload,
            EventPayload::Compaction { before: saved_before, .. } if *saved_before == before)));
        assert!(saved.events.iter().any(|event| matches!(&event.payload,
            EventPayload::Message { message, .. } if *message == old_request)));
        let (archived, total) = reopened.read_history("s1", 1, 10).unwrap();
        assert!(total > 0);
        assert_eq!(archived[0].1, old_request);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
