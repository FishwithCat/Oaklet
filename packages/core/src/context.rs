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
/// Tool schemas and the rebuilt system prompt are not included on the first
/// request; the provider anchor corrects for them as soon as one response
/// reports usage.
pub fn estimate_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_message).sum()
}

/// Token pressure for the next request, anchored on provider-reported usage.
///
/// `prompt_tokens` from the last response is exact for the request that
/// produced it. Messages appended after that request are priced locally and
/// added as a signed delta, so pruning or compaction that shrinks history also
/// lowers the reported pressure instead of pinning it at the old anchor.
#[derive(Debug, Default)]
pub struct Meter {
    anchor_prompt_tokens: Option<usize>,
    anchor_estimate: usize,
    pending_estimate: usize,
}

impl Meter {
    /// Record the estimate for the request about to be sent and return the
    /// pressure it is sent under.
    pub fn prepare(&mut self, messages: &[Value]) -> usize {
        let pressure = self.pressure(messages);
        self.pending_estimate = estimate_tokens(messages);
        pressure
    }

    /// Anchor the local estimate on provider-reported prompt tokens.
    pub fn record_prompt_tokens(&mut self, prompt_tokens: usize) {
        self.anchor_prompt_tokens = Some(prompt_tokens);
        self.anchor_estimate = self.pending_estimate;
    }

    /// Token pressure of the conversation as it stands.
    pub fn pressure(&self, messages: &[Value]) -> usize {
        let estimate = estimate_tokens(messages);
        match self.anchor_prompt_tokens {
            Some(anchor) => {
                let delta = estimate as i64 - self.anchor_estimate as i64;
                (anchor as i64 + delta).max(0) as usize
            }
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
        // The new message's local price, added to the exact anchor.
        assert_eq!(meter.pressure(&messages), 214);
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
