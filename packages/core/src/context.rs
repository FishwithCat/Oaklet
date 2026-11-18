//! Rough token accounting for the model-facing conversation.
//!
//! The agent loop needs a size signal before the provider reports anything, so
//! this module prices messages with a fixed heuristic. Once a response reports
//! `prompt_tokens`, [`Meter`] anchors on that exact number and prices only the
//! messages appended afterwards, which keeps the estimate close without a
//! tokenizer dependency.

use serde_json::Value;

/// Framing cost of one message: role, separators, and JSON punctuation.
const MESSAGE_OVERHEAD: usize = 4;

/// Framing cost of one multimodal content part.
const PART_OVERHEAD: usize = 4;

/// Rough cost of one attached image. Providers price a downscaled image near a
/// thousand tokens; pricing its base64 data URL as prose would overstate a
/// screenshot by orders of magnitude and trigger compaction right after paste.
const IMAGE_TOKENS: usize = 1_100;

/// ASCII prose packs about four characters per token.
const ASCII_CHARS_PER_TOKEN: usize = 4;

/// Estimate the tokens in one piece of model-visible text.
///
/// ASCII is priced at four characters per token. Every non-ASCII character is
/// priced at one token: CJK text runs near one token per character, and pricing
/// it at a quarter token would let a Chinese conversation overflow long before
/// the estimate noticed. The rule is deliberately pessimistic for other scripts.
pub fn estimate_text(text: &str) -> usize {
    let mut ascii = 0usize;
    let mut wide = 0usize;
    for character in text.chars() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    ascii.div_ceil(ASCII_CHARS_PER_TOKEN) + wide
}

/// Estimate the tokens in one OpenAI-style chat message.
pub(crate) fn estimate_message(message: &Value) -> usize {
    let mut tokens = MESSAGE_OVERHEAD;
    match message.get("content") {
        Some(Value::String(text)) => tokens += estimate_text(text),
        Some(Value::Array(parts)) => {
            for part in parts {
                tokens += PART_OVERHEAD;
                tokens += match part.get("type").and_then(Value::as_str) {
                    Some("image_url" | "input_image") => IMAGE_TOKENS,
                    _ => match part.get("text").and_then(Value::as_str) {
                        Some(text) => estimate_text(text),
                        None => estimate_text(&part.to_string()),
                    },
                };
            }
        }
        _ => {}
    }
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
        tokens += estimate_text(reasoning);
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            tokens += PART_OVERHEAD;
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                tokens += estimate_text(name);
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
                tokens += estimate_text(arguments);
            }
        }
    }
    if let Some(call_id) = message.get("tool_call_id").and_then(Value::as_str) {
        tokens += estimate_text(call_id);
    }
    tokens
}

/// Estimate the tokens in the whole message array sent to the model.
///
/// Includes the system message; tool schemas are priced separately by `Meter`.
pub fn estimate_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_message).sum()
}

/// Stable identities belong to the application and never reach a provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageId {
    Stored(i64),
    Pending(usize),
    Checkpoint,
    Repair(String),
}

pub(crate) fn identify(mut message: Value, id: MessageId) -> Value {
    message["oaklet_id"] = serde_json::to_value(id).unwrap();
    message
}

pub(crate) fn message_id(message: &Value) -> Result<MessageId, String> {
    serde_json::from_value(message["oaklet_id"].clone())
        .map_err(|_| "A context message has no stable identity; nothing was compacted.".into())
}

pub(crate) fn source(mut message: Value, source: &str) -> Value {
    message["oaklet_source"] = Value::String(source.into());
    message
}

pub(crate) fn is_request(message: &Value) -> bool {
    message["role"] == "user"
        && !crate::store::is_checkpoint(message)
        && !matches!(
            message["oaklet_source"].as_str(),
            Some("reminder" | "tool_images")
        )
}

pub(crate) fn provider_message(mut message: Value) -> Value {
    if let Some(object) = message.as_object_mut() {
        object.remove("oaklet_checkpoint");
        object.remove("oaklet_id");
        object.remove("oaklet_source");
    }
    message
}

/// Merge adjacent user messages only at the provider boundary.
pub(crate) fn request_messages(messages: &[Value]) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    for message in messages.iter().cloned().map(provider_message) {
        if message["role"] == "user" && output.last().is_some_and(|last| last["role"] == "user") {
            let previous = output.last_mut().unwrap();
            let parts = |content: &Value| match content {
                Value::Array(parts) => parts.clone(),
                Value::String(text) => vec![serde_json::json!({"type":"text", "text":text})],
                _ => Vec::new(),
            };
            let mut content = parts(&previous["content"]);
            content.extend(parts(&message["content"]));
            previous["content"] = Value::Array(content);
        } else {
            output.push(message);
        }
    }
    output
}

pub(crate) fn output_reserve(window: usize) -> usize {
    32_768.min(window / 4)
}

pub(crate) fn input_limit(window: usize) -> usize {
    window.saturating_sub(output_reserve(window))
}

pub(crate) fn request_tokens(messages: &[Value], schemas: &Value) -> usize {
    estimate_tokens(&request_messages(messages)) + estimate_text(&schemas.to_string())
}

/// Token pressure for the next request, anchored on provider-reported usage.
///
/// `prompt_tokens` from the last response is exact for the request that
/// produced it. Messages appended after that request are priced locally and
/// added as a signed delta. History replacement invalidates the anchor;
/// usage from the next successful request establishes a fresh one.
#[derive(Debug, Default)]
pub struct Meter {
    anchor_prompt_tokens: Option<usize>,
    anchor_estimate: usize,
    pending_estimate: usize,
    schema_tokens: usize,
}

impl Meter {
    pub fn invalidate(&mut self) {
        self.anchor_prompt_tokens = None;
    }

    pub fn set_schema_tokens(&mut self, tokens: usize) {
        self.schema_tokens = tokens;
    }

    /// Record the estimate for the request about to be sent and return the
    /// pressure it is sent under.
    pub fn prepare(&mut self, messages: &[Value]) -> usize {
        let pressure = self.pressure(messages);
        self.pending_estimate =
            estimate_tokens(&request_messages(messages)).saturating_add(self.schema_tokens);
        pressure
    }

    /// Anchor the local estimate on provider-reported prompt tokens.
    pub fn record_prompt_tokens(&mut self, prompt_tokens: usize) {
        self.anchor_prompt_tokens = Some(prompt_tokens);
        self.anchor_estimate = self.pending_estimate;
    }

    /// Token pressure of the conversation as it stands.
    pub fn pressure(&self, messages: &[Value]) -> usize {
        let estimate =
            estimate_tokens(&request_messages(messages)).saturating_add(self.schema_tokens);
        match self.anchor_prompt_tokens {
            Some(anchor) => anchor
                .saturating_add(estimate.saturating_sub(self.anchor_estimate))
                .saturating_sub(self.anchor_estimate.saturating_sub(estimate)),
            None => estimate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    #[test]
    fn prices_ascii_at_four_characters_per_token() {
        assert_eq!(estimate_text(""), 0);
        assert_eq!(estimate_text("abc"), 1);
        assert_eq!(estimate_text("abcd"), 1);
        assert_eq!(estimate_text("abcde"), 2);
    }

    #[test]
    fn prices_non_ascii_at_one_token_per_character() {
        assert_eq!(estimate_text("你好世界"), 4);
        // Three ASCII characters round up to one token, then two wide ones.
        assert_eq!(estimate_text("abc你好"), 3);
    }

    #[test]
    fn prices_content_reasoning_and_tool_calls() {
        let plain = estimate_message(&json!({ "role": "user", "content": "hello" }));
        let tool_call = estimate_message(&json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "thinking",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}" }
            }]
        }));
        assert!(tool_call > plain);
    }

    #[test]
    fn prices_an_image_part_below_its_data_url_size() {
        let message = json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "what does this error say?" },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{}", "A".repeat(400_000))
                    }
                }
            ]
        });
        let tokens = estimate_message(&message);
        assert!(
            tokens > IMAGE_TOKENS,
            "{tokens} should include the text part"
        );
        assert!(
            tokens < 2_000,
            "{tokens} should not price the base64 payload"
        );
    }

    #[test]
    fn meter_starts_with_the_local_estimate() {
        let messages = vec![message(&"a".repeat(400))];
        let mut meter = Meter::default();
        assert_eq!(meter.prepare(&messages), 104);
    }

    #[test]
    fn meter_anchors_on_reported_usage_and_prices_only_new_messages() {
        let mut messages = vec![message(&"a".repeat(400))];
        let mut meter = Meter::default();
        meter.prepare(&messages);
        meter.record_prompt_tokens(200);

        messages.push(message(&"b".repeat(40)));
        // The merged multimodal envelope includes text-part framing.
        assert_eq!(meter.pressure(&messages), 218);
    }

    #[test]
    fn meter_follows_history_that_shrinks_after_pruning() {
        let mut messages = vec![message(&"a".repeat(400))];
        let mut meter = Meter::default();
        meter.prepare(&messages);
        meter.record_prompt_tokens(200);

        messages = vec![message(&"a".repeat(40))];
        assert_eq!(meter.pressure(&messages), 110);
    }
}

#[cfg(test)]
mod request_budget_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schemas_are_counted_before_usage_and_as_a_delta_after_usage() {
        let messages = vec![json!({"role":"system", "content":"a".repeat(400)})];
        let mut meter = Meter::default();
        meter.set_schema_tokens(1000);
        assert_eq!(meter.prepare(&messages), 1104);
        meter.record_prompt_tokens(1200);
        assert_eq!(meter.pressure(&messages), 1200);
        meter.set_schema_tokens(1300);
        assert_eq!(meter.pressure(&messages), 1500);
        meter.prepare(&messages);
        meter.record_prompt_tokens(1500);
        assert_eq!(meter.pressure(&messages), 1500);
        meter.set_schema_tokens(1000);
        assert_eq!(meter.pressure(&messages), 1200);
    }

    #[test]
    fn checkpoint_identity_is_removed_from_provider_messages() {
        let message = crate::store::checkpoint(json!({"role":"user", "content":"checkpoint"}));
        assert!(crate::store::is_checkpoint(&message));
        assert!(
            request_messages(&[message])[0]
                .get("oaklet_checkpoint")
                .is_none()
        );
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replacing_history_invalidates_both_positive_and_negative_usage_error() {
        for usage in [4_000, 16_000] {
            let old = vec![json!({"role":"user", "content":"x".repeat(39984)})];
            let new = vec![json!({"role":"user", "content":"new context"})];
            let mut meter = Meter::default();
            meter.prepare(&old);
            meter.record_prompt_tokens(usage);
            meter.invalidate();
            assert_eq!(
                meter.prepare(&new),
                estimate_tokens(&request_messages(&new))
            );
        }
    }

    #[test]
    fn internal_identity_and_sources_are_removed_after_user_message_merging() {
        let checkpoint = identify(
            crate::store::checkpoint(json!({"role":"user","content":"summary"})),
            MessageId::Checkpoint,
        );
        let request = identify(
            source(
                json!({"role":"user","content":[{"type":"text","text":"request"}]}),
                "human",
            ),
            MessageId::Stored(3),
        );
        let messages = request_messages(&[checkpoint, request]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[0].as_object().unwrap().len(), 2);
    }
}
