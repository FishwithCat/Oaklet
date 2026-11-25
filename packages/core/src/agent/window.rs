//! All context changes enter through one reducer and one recording path.
use crate::chat::RunContext;
use crate::context::Meter;
use crate::tools::{Cancel, Tool, ToolRegistry};
use crate::{AppConfig, compaction, context};
use serde_json::Value;

use super::SystemPrompt;

#[allow(clippy::too_many_arguments)]
pub(super) async fn make_room(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    meter: &mut Meter,
    prompt: &mut SystemPrompt,
    cancel: &Cancel,
    ctx: &RunContext,
) -> Result<(), String> {
    meter.set_schema_tokens(context::estimate_text(&tools.schemas().to_string()));
    if meter.prepare(messages) <= compaction::trigger_at(config.context_window) {
        return Ok(());
    }
    let changed = reduce(
        client,
        config,
        tools,
        messages,
        prompt,
        cancel,
        ctx,
        compaction::Trigger::Automatic,
    )
    .await?;
    if changed {
        meter.invalidate();
    }
    meter.set_schema_tokens(context::estimate_text(&tools.schemas().to_string()));
    if meter.prepare(messages) > context::input_limit(config.context_window) {
        return Err("The current request and tools do not fit the context window with room for a response. History was preserved; reduce the input or increase the window.".into());
    }
    Ok(())
}

pub(super) async fn recover(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    prompt: &mut SystemPrompt,
    cancel: &Cancel,
    ctx: &RunContext,
) -> Result<bool, String> {
    reduce(
        client,
        config,
        tools,
        messages,
        prompt,
        cancel,
        ctx,
        compaction::Trigger::Overflow,
    )
    .await
}

async fn apply(
    mut next: Vec<Value>,
    messages: &mut Vec<Value>,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    prompt: &mut SystemPrompt,
    ctx: &RunContext,
) -> Result<(), String> {
    let before = context::request_tokens(messages, &tools.schemas());
    if tools.get("history_read").is_none() {
        let history = crate::tools::history::HistoryRead::new(
            ctx.store(),
            ctx.session_id().into(),
            ctx.archived(),
        );
        let Some(message) = next
            .first_mut()
            .filter(|message| message["role"] == "system")
        else {
            return Err("The agent system prompt is unavailable; history was preserved.".into());
        };
        prompt.append(history.guidance());
        message["content"] = Value::String(prompt.render());
        tools.push(Box::new(history));
    }
    let after = context::request_tokens(&next, &tools.schemas());
    if after >= before {
        return Err(
            "The replacement and history tool would not shrink the request; history was preserved."
                .into(),
        );
    }
    *messages = next;
    ctx.mark_compacted(before, after);
    ctx.record_archived(messages).await?;
    ctx.context(super::context_usage(
        messages,
        tools,
        prompt,
        after,
        config.context_window,
    ));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn reduce(
    client: &reqwest::Client,
    config: &AppConfig,
    tools: &mut ToolRegistry,
    messages: &mut Vec<Value>,
    prompt: &mut SystemPrompt,
    cancel: &Cancel,
    ctx: &RunContext,
    trigger: compaction::Trigger,
) -> Result<bool, String> {
    if cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let system = messages
        .first()
        .filter(|message| message["role"] == "system")
        .map(context::estimate_message)
        .unwrap_or(0);
    if system + context::estimate_text(&tools.schemas().to_string())
        >= context::input_limit(config.context_window)
    {
        return Err("The system prompt and tools leave no room for the request and response; reduce tools or increase the context window.".into());
    }
    let mut progress = false;
    if let Some(trimmed) = compaction::trim_tool_outputs(messages) {
        apply(trimmed, messages, config, tools, prompt, ctx).await?;
        progress = true;
        if context::request_tokens(messages, &tools.schemas())
            <= compaction::trigger_at(config.context_window)
        {
            return Ok(true);
        }
    }
    if let Some(next) =
        compaction::compact(client, config, messages, tools, trigger, cancel).await?
    {
        apply(next, messages, config, tools, prompt, ctx).await?;
        progress = true;
    }
    if context::request_tokens(messages, &tools.schemas())
        > context::input_limit(config.context_window)
    {
        return Err("The compacted context still does not fit. The successful reduction was saved; retry or increase the context window.".into());
    }
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::test_support::state;
    use crate::compaction::test_support::{summary_server_with, user};
    use crate::context::{MessageId, identify};
    use crate::store::NewUserTurn;
    use crate::tools::CancelHandle;
    use serde_json::json;

    #[tokio::test]
    async fn a_failed_summary_saves_prior_trims_and_the_partial_reply_without_losing_originals() {
        let (url, _, server) = summary_server_with(vec![(500, "unavailable")]);
        let config = AppConfig {
            base_url: url,
            context_window: 8192,
            ..AppConfig::default()
        };
        let (state, store, root) = state(config.clone());
        store.create_session("s", "/tmp", 1).unwrap();
        let request = user("CURRENT REQUEST: never commit");
        let started = store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: request.clone(),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let ctx = RunContext::without_window(state, "s", "r", started);
        let mut messages = vec![
            json!({"role":"system","content":"system"}),
            identify(request.clone(), MessageId::Stored(1)),
        ];
        for index in 0..8 {
            let id = format!("c{index}");
            messages.push(ctx.record_message(json!({"role":"assistant","tool_calls":[{"id":id,"type":"function","function":{"name":"read_file","arguments":"{}"}}]}),None));
            ctx.tool_start(&id, "read_file", "read", &[]);
            messages.push(ctx.tool_result(
                &id,
                true,
                &format!("result {index}: {}", "x".repeat(8000)),
                &[],
                None,
            ));
        }
        let original = messages.clone();
        let (_handle, cancel) = CancelHandle::new();
        let mut prompt = crate::agent::system_prompt(&config, &ToolRegistry::new(), &[], None);
        let error = make_room(
            &reqwest::Client::new(),
            &config,
            &mut ToolRegistry::new(),
            &mut messages,
            &mut Meter::default(),
            &mut prompt,
            &cancel,
            &ctx,
        )
        .await
        .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("500"));
        assert_eq!(messages.len(), original.len());
        assert_eq!(messages[1], original[1]);
        assert!(context::estimate_tokens(&messages) < context::estimate_tokens(&original));
        ctx.delta("A partially streamed reply");
        ctx.finish_error(messages[1..].to_vec(), error).await;
        let (restored, _) = store.context_snapshot("s").unwrap();
        assert_eq!(restored.len(), original.len()); // no system, plus the partial reply
        assert_eq!(
            restored.last().unwrap()["content"],
            "A partially streamed reply"
        );
        assert_eq!(restored[0], identify(request, MessageId::Stored(1)));
        let (archived, total) = store.read_history("s", 1, 50).unwrap();
        assert_eq!(total, 8);
        assert!(archived.iter().all(|(_, message)| {
            message["content"]
                .as_str()
                .unwrap()
                .contains(&"x".repeat(8000))
        }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn an_overflow_retry_uses_the_same_reducer_and_persists_its_exact_surface() {
        let (url, seen, server) = summary_server_with(vec![
            (
                400,
                r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
            ),
            (
                200,
                r#"{"choices":[{"finish_reason":"stop","message":{"content":"Goal: finish. Old investigation is complete."}}]}"#,
            ),
            (
                200,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            ),
        ]);
        let config = AppConfig {
            base_url: url,
            context_window: 32768,
            ..AppConfig::default()
        };
        let (state, store, root) = state(config.clone());
        store.create_session("s", "/tmp", 1).unwrap();
        let request = user("CURRENT REQUEST");
        let started = store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: request.clone(),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let ctx = RunContext::without_window(state, "s", "r", started);
        let mut prompt = crate::agent::system_prompt(&config, &ToolRegistry::new(), &[], None);
        let system_text = prompt.render();
        let mut messages = vec![
            json!({"role":"system","content":system_text.clone()}),
            identify(request.clone(), MessageId::Stored(1)),
        ];
        messages.push(ctx.record_message(
            json!({"role":"assistant","content":"old investigation ".repeat(1000)}),
            None,
        ));
        messages
            .push(ctx.record_message(json!({"role":"assistant","content":"latest step"}), None));
        let mut tools = ToolRegistry::new();
        let (_handle, cancel) = CancelHandle::new();
        let mut retried = false;
        assert!(matches!(
            crate::agent::step::request(
                &reqwest::Client::new(),
                &config,
                &mut tools,
                &mut messages,
                &mut prompt,
                &cancel,
                &ctx,
                &mut retried
            )
            .await
            .unwrap(),
            crate::agent::step::Next::Retry
        ));
        let reply = crate::agent::step::request(
            &reqwest::Client::new(),
            &config,
            &mut tools,
            &mut messages,
            &mut prompt,
            &cancel,
            &ctx,
            &mut retried,
        )
        .await
        .unwrap();
        let crate::agent::step::Next::Reply(reply) = reply else {
            panic!("expected reply")
        };
        messages
            .push(ctx.record_message(json!({"role":"assistant","content":reply.content}), None));
        ctx.finish_done(messages[1..].to_vec()).await;
        server.join().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3);
        let retry: Value = serde_json::from_str(&seen.lock().unwrap()[2]).unwrap();
        let expected_after =
            context::request_tokens(retry["messages"].as_array().unwrap(), &retry["tools"]);
        let session = store.get_session("s").unwrap().unwrap();
        let recorded_after = session.events.iter().find_map(|event| match event.payload {
            crate::store::EventPayload::Context { after, .. } => Some(after),
            _ => None,
        });
        assert_eq!(recorded_after, Some(expected_after));
        let guidance = tools.get("history_read").unwrap().guidance();
        let retry_system = retry["messages"][0]["content"].as_str().unwrap();
        assert!(retry_system.starts_with(&system_text));
        assert_eq!(retry_system.matches(guidance).count(), 1);
        let (restored, _) = store.context_snapshot("s").unwrap();
        assert!(crate::store::is_checkpoint(&restored[0]));
        assert_eq!(restored[1], identify(request, MessageId::Stored(1)));
        assert_eq!(restored.last().unwrap()["content"], "Done");
        assert_eq!(store.history_count("s").unwrap(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn stopping_after_compaction_preserves_a_repaired_parallel_tool_batch() {
        let (url, _, server) =
            summary_server_with(vec![(200, crate::compaction::test_support::SUMMARY_BODY)]);
        let config = AppConfig {
            base_url: url,
            context_window: 32768,
            ..AppConfig::default()
        };
        let (state, store, root) = state(config.clone());
        store.create_session("s", "/tmp", 1).unwrap();
        let request = user("Never commit");
        let started = store
            .begin_run(
                "s",
                "r",
                &NewUserTurn {
                    message: request.clone(),
                    display: None,
                    title: None,
                },
                2,
            )
            .unwrap();
        let ctx = RunContext::without_window(state, "s", "r", started);
        let mut messages = vec![
            json!({"role":"system","content":"system"}),
            identify(request, MessageId::Stored(1)),
        ];
        messages.push(ctx.record_message(
            json!({"role":"assistant","content":"old investigation ".repeat(1000)}),
            None,
        ));
        messages
            .push(ctx.record_message(json!({"role":"assistant","content":"latest step"}), None));
        let (_handle, cancel) = CancelHandle::new();
        let mut prompt = crate::agent::system_prompt(&config, &ToolRegistry::new(), &[], None);
        assert!(
            recover(
                &reqwest::Client::new(),
                &config,
                &mut ToolRegistry::new(),
                &mut messages,
                &mut prompt,
                &cancel,
                &ctx
            )
            .await
            .unwrap()
        );
        server.join().unwrap();
        messages.push(ctx.record_message(
            json!({"role":"assistant","content":"", "tool_calls":[
                {"id":"c1","type":"function","function":{"name":"read_file","arguments":"{}"}},
                {"id":"c2","type":"function","function":{"name":"read_file","arguments":"{}"}}
            ]}),
            None,
        ));
        ctx.tool_start("c1", "read_file", "read", &[]);
        ctx.delta("Interrupted reply");
        ctx.finish_stopped(messages[1..].to_vec()).await;
        let (restored, _) = store.context_snapshot("s").unwrap();
        let call = restored
            .iter()
            .position(|message| message.get("tool_calls").is_some())
            .unwrap();
        assert_eq!(restored[call + 1]["tool_call_id"], "c1");
        assert_eq!(restored[call + 2]["tool_call_id"], "c2");
        assert_eq!(restored.last().unwrap()["content"], "Interrupted reply");
        assert!(
            restored
                .iter()
                .all(|message| context::message_id(message).is_ok())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
