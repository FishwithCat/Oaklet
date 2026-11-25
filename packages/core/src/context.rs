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
    require_reasoning_replay(&mut message);
    message
}

fn require_reasoning_replay(message: &mut Value) {
    if message["role"] == "assistant"
        && message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .is_none()
    {
        message["reasoning_content"] = Value::String(String::new());
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    System,
    Instructions,
    Skills,
    Tools,
    Conversation,
    ToolResults,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSegment {
    pub kind: ContextKind,
    pub tokens: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptSections {
    pub system: usize,
    pub skills: usize,
    pub instructions: usize,
}

pub fn breakdown(
    messages: &[Value],
    schemas: &Value,
    sections: PromptSections,
) -> Vec<ContextSegment> {
    let mut conversation = 0;
    let mut results = 0;
    let skip = usize::from(
        messages
            .first()
            .is_some_and(|message| message["role"] == "system"),
    );
    for message in messages.iter().skip(skip) {
        let tokens = estimate_message(message);
        let images = message["oaklet_source"].as_str() == Some("tool_images");
        if message["role"] == "tool" || images {
            results += tokens;
        } else {
            conversation += tokens;
        }
    }
    let segments = vec![
        ContextSegment {
            kind: ContextKind::System,
            tokens: sections.system,
        },
        ContextSegment {
            kind: ContextKind::Instructions,
            tokens: sections.instructions,
        },
        ContextSegment {
            kind: ContextKind::Skills,
            tokens: sections.skills,
        },
        ContextSegment {
            kind: ContextKind::Tools,
            tokens: estimate_text(&schemas.to_string()),
        },
        ContextSegment {
            kind: ContextKind::Conversation,
            tokens: conversation,
        },
        ContextSegment {
            kind: ContextKind::ToolResults,
            tokens: results,
        },
    ];
    segments
        .into_iter()
        .filter(|segment| segment.tokens > 0)
        .collect()
}

pub fn scaled(segments: Vec<ContextSegment>, total: usize) -> Vec<ContextSegment> {
    let sum: usize = segments.iter().map(|segment| segment.tokens).sum();
    if sum == 0 || total == 0 {
        return segments
            .into_iter()
            .filter(|segment| segment.tokens > 0)
            .collect();
    }
    let tokens = distribute(
        &segments
            .iter()
            .map(|segment| segment.tokens)
            .collect::<Vec<_>>(),
        total,
    );
    let scaled: Vec<ContextSegment> = segments
        .into_iter()
        .zip(tokens)
        .map(|(mut segment, tokens)| {
            segment.tokens = tokens;
            segment
        })
        .collect();
    scaled
        .into_iter()
        .filter(|segment| segment.tokens > 0)
        .collect()
}

fn distribute(values: &[usize], total: usize) -> Vec<usize> {
    let sum: usize = values.iter().sum();
    if sum == 0 {
        return vec![0; values.len()];
    }
    let mut counts: Vec<usize> = values.iter().map(|value| value * total / sum).collect();
    let assigned: usize = counts.iter().sum();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(values[index] * total % sum));
    for index in order.into_iter().take(total.saturating_sub(assigned)) {
        counts[index] += 1;
    }
    counts
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
    fn every_replayed_assistant_turn_carries_reasoning_content() {
        let messages = vec![
            json!({"role":"user", "content":"go"}),
            json!({"role":"assistant", "content":"first answer"}),
            json!({"role":"user", "content":"again"}),
            json!({
                "role":"assistant",
                "content":"",
                "reasoning_content":"checked the path",
                "tool_calls":[{
                    "id":"call_1",
                    "type":"function",
                    "function": {"name":"read_file", "arguments":"{}"}
                }]
            }),
            json!({"role":"tool", "tool_call_id":"call_1", "content":"ok"}),
            json!({"role":"assistant", "content":"done", "reasoning_content": null}),
        ];
        let replayed = request_messages(&messages);
        assert_eq!(replayed.len(), messages.len());
        assert_eq!(replayed[1]["reasoning_content"], "");
        assert_eq!(replayed[3]["reasoning_content"], "checked the path");
        assert_eq!(replayed[5]["reasoning_content"], "");
        for index in [0, 2, 4] {
            assert!(
                replayed[index].get("reasoning_content").is_none(),
                "{} must not gain a reasoning field",
                replayed[index]
            );
        }
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

#[cfg(test)]
mod breakdown_tests {
    use super::*;
    use serde_json::json;

    fn segments_total(segments: &[ContextSegment]) -> usize {
        segments.iter().map(|segment| segment.tokens).sum()
    }

    #[test]
    fn attributes_a_request_to_its_parts() {
        let messages = vec![
            json!({"role":"system", "content":"the prompt"}),
            json!({"role":"user", "content":"hello"}),
            json!({"role":"assistant", "content":"", "tool_calls":[{
                "id":"c1", "type":"function",
                "function": {"name":"read_file", "arguments":"{\"path\":\"src/main.rs\"}"}
            }]}),
            json!({"role":"tool", "tool_call_id":"c1", "content":"fn main() {}"}),
            json!({"role":"user", "content":[{"type":"image_url","image_url":{"url":"data:"}}],
                   "oaklet_source":"tool_images"}),
        ];
        let schemas = json!([{"type":"function","function":{"name":"read_file"}}]);
        let sections = PromptSections {
            system: 100,
            skills: 20,
            instructions: 30,
        };
        let segments = breakdown(&messages, &schemas, sections);
        let kinds: Vec<_> = segments.iter().map(|segment| segment.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ContextKind::System,
                ContextKind::Instructions,
                ContextKind::Skills,
                ContextKind::Tools,
                ContextKind::Conversation,
                ContextKind::ToolResults,
            ]
        );
        assert_eq!(segments[0].tokens, 100);
        assert_eq!(segments[1].tokens, 30);
        assert_eq!(segments[2].tokens, 20);
        assert_eq!(segments[3].tokens, estimate_text(&schemas.to_string()));
        assert_eq!(
            segments[4].tokens,
            estimate_message(&messages[1]) + estimate_message(&messages[2])
        );
        assert_eq!(
            segments[5].tokens,
            estimate_message(&messages[3]) + estimate_message(&messages[4])
        );
    }

    #[test]
    fn drops_segments_the_request_does_not_carry() {
        let messages = vec![json!({"role":"system", "content":"the prompt"})];
        let schemas = json!([]);
        let sections = PromptSections {
            system: 100,
            ..PromptSections::default()
        };
        let segments = breakdown(&messages, &schemas, sections);
        let kinds: Vec<_> = segments.iter().map(|segment| segment.kind).collect();
        assert_eq!(kinds, vec![ContextKind::System, ContextKind::Tools]);
    }

    #[test]
    fn scales_segments_to_the_provider_total() {
        let segments = vec![
            ContextSegment {
                kind: ContextKind::System,
                tokens: 300,
            },
            ContextSegment {
                kind: ContextKind::ToolResults,
                tokens: 700,
            },
        ];
        let scaled = scaled(segments, 1_000_000);
        assert_eq!(segments_total(&scaled), 1_000_000);
        assert_eq!(scaled[0].tokens, 300_000);
        assert_eq!(scaled[1].tokens, 700_000);
    }

    #[test]
    fn keeps_estimate_unchanged_when_it_already_matches_the_total() {
        let segments = vec![ContextSegment {
            kind: ContextKind::Conversation,
            tokens: 40,
        }];
        let scaled = scaled(segments.clone(), 40);
        assert_eq!(scaled, segments);
    }
}
