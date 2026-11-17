use super::*;
use crate::compaction::MIN_REPLACED;
use crate::compaction::test_support::*;

#[test]
fn pressure_then_force_trimming_keeps_the_spill_pointer() {
    let path = "/tmp/oaklet/spill/session-0123456789abcdef/read_file-call_1.txt";
    let text = format!(
        "{}\n\n[... tool output middle pruned; full output (900 lines, 20000 bytes) saved to: {path}. Use read_file with offset and limit to page through it.]\n\n{}",
        "h".repeat(4_000),
        "t".repeat(1_500)
    );
    let pressure = trim_head_and_tail(
        &text,
        PRESSURE_TOOL_OUTPUT_CHARS,
        PRESSURE_TOOL_OUTPUT_HEAD_CHARS,
        PRESSURE_TOOL_OUTPUT_TAIL_CHARS,
    );
    let forced = trim_head_and_tail(
        &pressure,
        FORCE_TOOL_OUTPUT_CHARS,
        FORCE_TOOL_OUTPUT_HEAD_CHARS,
        FORCE_TOOL_OUTPUT_TAIL_CHARS,
    );

    assert_eq!(spill_pointer(&forced), Some(path));
    assert!(forced.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
}

#[test]
fn trims_oversized_tool_output_for_recovery() {
    let original = "x".repeat(20_000);
    let messages = vec![system(), user("go"), tool("call_1", &original)];

    let (trimmed, replaced) = trim_tool_outputs(
        &messages,
        FORCE_TOOL_OUTPUT_CHARS,
        FORCE_TOOL_OUTPUT_HEAD_CHARS,
        FORCE_TOOL_OUTPUT_TAIL_CHARS,
    );
    let text = trimmed[2]["content"].as_str().unwrap();

    assert!(text.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
    assert!(text.contains(FORCE_TOOL_OUTPUT_MARKER));
    assert!(text.contains("history_read"));
    assert!(text.starts_with('x') && text.ends_with('x'));
    assert_eq!(replaced.into_iter().collect::<Vec<_>>(), vec![2]);
    assert_eq!(messages[2]["content"].as_str().unwrap(), original);
}

#[test]
fn keeps_an_existing_spill_pointer_when_trimming_again() {
    let path = "/tmp/oaklet/spill/session-0123456789abcdef/run_command-call_1.txt";
    let original = format!(
        "artifact saved to: build/report.html\n{}\n\n[... tool output middle pruned; full output (900 lines, 20000 bytes) saved to: {path}. Use read_file with offset and limit to page through it.]\n\n{}",
        "h".repeat(4_000),
        "t".repeat(4_000)
    );

    let trimmed = trim_head_and_tail(
        &original,
        FORCE_TOOL_OUTPUT_CHARS,
        FORCE_TOOL_OUTPUT_HEAD_CHARS,
        FORCE_TOOL_OUTPUT_TAIL_CHARS,
    );

    assert!(trimmed.contains(path));
    assert!(trimmed.chars().count() <= FORCE_TOOL_OUTPUT_CHARS);
}

#[test]
fn never_starts_the_tail_with_an_orphan_tool_message() {
    let mut messages = vec![system()];
    for step in 0..12 {
        messages.push(user(&format!("request {step}")));
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": format!("call_{step}"),
                "type": "function",
                "function": { "name": "read_file", "arguments": "{}" }
            }]
        }));
        messages.push(tool(&format!("call_{step}"), &"x".repeat(800)));
    }

    let cut = select_cut(&messages, 500, 100).unwrap();
    assert_ne!(role(&messages[cut]), Some("tool"));
    assert!(estimate_tokens(&messages[cut..]) <= 500);
    assert!(cut >= 2);
}

#[test]
fn skips_a_conversation_with_nothing_old_enough_to_replace() {
    let messages = vec![system(), user("hi"), assistant("hello")];
    assert_eq!(select_cut(&messages, 100_000, MIN_REPLACED), None);
}

#[test]
fn never_summarizes_the_latest_user_request() {
    let mut messages = vec![system()];
    for step in 0..20 {
        messages.push(user(&format!("old {step} {}", "x".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user("CURRENT REQUEST"));
    messages.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call_now",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{}" }
        }]
    }));
    messages.push(tool("call_now", &"r".repeat(400)));

    let cut = select_cut(&messages, 500, 100).unwrap();
    assert!(cut <= messages.len() - 3);
    assert!(
        messages[cut..]
            .iter()
            .any(|message| message["content"] == "CURRENT REQUEST")
    );
}

#[test]
fn a_loop_guard_reminder_does_not_replace_the_current_request() {
    let mut messages = vec![system()];
    for step in 0..20 {
        messages.push(user(&format!("old {step} {}", "x".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user("CURRENT REQUEST"));
    messages.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call_now",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{}" }
        }]
    }));
    messages.push(tool("call_now", &"r".repeat(400)));
    messages.push(user(&format!(
        "{}you have called `read_file` 3 times in a row",
        crate::repeat::REMINDER_PREFIX
    )));
    messages.push(assistant("trying another approach"));

    let cut = select_cut(&messages, 500, 100).unwrap();
    assert!(
        messages[cut..]
            .iter()
            .any(|message| message["content"] == "CURRENT REQUEST")
    );
}

#[test]
fn a_tool_image_turn_does_not_replace_the_current_request() {
    let mut messages = vec![system()];
    for step in 0..20 {
        messages.push(user(&format!("old {step} {}", "x".repeat(800))));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user("CURRENT REQUEST"));
    messages.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call_now",
            "type": "function",
            "function": { "name": "read_image", "arguments": "{}" }
        }]
    }));
    messages.push(tool("call_now", "image result"));
    messages.push(json!({
        "role": "user",
        "content": [
            {
                "type": "text",
                "text": "Images returned by the previous tool call(s). This is tool output, not a new user request."
            },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
        ]
    }));

    assert_eq!(
        messages
            .iter()
            .rposition(|message| message["content"] == "CURRENT REQUEST"),
        Some(41)
    );
    assert_eq!(latest_user(&messages), 41);

    let cut = select_cut(&messages, 500, 100).unwrap();
    assert!(cut <= 41);
    assert!(
        messages[cut..]
            .iter()
            .any(|message| message["content"] == "CURRENT REQUEST")
    );
}

#[test]
fn falls_back_to_the_current_turn_when_it_exceeds_the_retain_budget() {
    let mut messages = vec![system()];
    for _ in 0..5 {
        messages.push(user(&"old ".repeat(100)));
        messages.push(assistant(&"a".repeat(800)));
    }
    messages.push(user(&"x".repeat(20_000)));

    let cut = select_cut(&messages, 100, 100).unwrap();
    assert_eq!(cut, messages.len() - 1);
    assert_eq!(role(&messages[cut]), Some("user"));
}
