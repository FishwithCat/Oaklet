use super::*;
use crate::AppConfig;
use crate::compaction::test_support::*;
use crate::compaction::{CompactMode, compact_at};
use crate::context::estimate_text;

#[tokio::test]
async fn first_fallback_keeps_user_constraints_outside_the_log_budget() {
    let constraint = "Use staging only; never deploy to production. 保留旧格式，禁止提交。";
    let messages = vec![
        system(),
        user("Fix the build."),
        assistant(&"old log ".repeat(3_000)),
        json!({"role":"user", "content":[{"type":"text", "text":constraint}]}),
        assistant(&"new log ".repeat(3_000)),
        user("Continue."),
    ];
    let config = AppConfig {
        context_window: 8_192,
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let compacted = crate::compaction::compact(
        &reqwest::Client::new(),
        &config,
        &messages,
        estimate_tokens(&messages),
        &crate::tools::ToolRegistry::new(),
        &cancel,
    )
    .await
    .unwrap()
    .unwrap();
    let checkpoint = compacted.messages[1]["content"].as_str().unwrap();
    assert!(checkpoint.contains("Fix the build."));
    assert!(checkpoint.contains(constraint));
    assert!(checkpoint.ends_with("Continue."));
    assert!(estimate_tokens(&compacted.messages) < estimate_tokens(&messages));
    assert_compaction_restores(&messages, &compacted);

    let mut messages = compacted.messages;
    for _ in 0..3 {
        messages.push(assistant(&"more log ".repeat(3_000)));
        messages.push(user("Continue."));
        let compacted = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            messages.len() - 1,
            None,
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        let checkpoint = compacted.messages[1]["content"].as_str().unwrap();
        assert_eq!(checkpoint.matches("<checkpoint-requests>").count(), 1);
        assert_eq!(checkpoint.matches(constraint).count(), 1);
        assert!(checkpoint.contains("Fix the build."));
        messages = compacted.messages;
    }
}

#[test]
fn mechanical_digest_preserves_old_checkpoints_and_merged_requests() {
    let merged = assemble(
        &[
            system(),
            user("old"),
            assistant("done"),
            json!({"role": "user", "content": [{"type": "text", "text": "REAL USER REQUIREMENT"}]}),
        ],
        3,
        "Never commit without approval",
    );
    let mut messages = merged[1..].to_vec();
    messages.push(json!({
        "role": "assistant", "content": format!("{}TEXT BEYOND CAP", "a".repeat(4_000)),
        "tool_calls": [{"function": {"name": "read_file", "arguments": format!("{}\nARGS BEYOND CAP", "x".repeat(500))}}]
    }));
    let digest = mechanical_digest(&messages, None);

    assert!(digest.contains("REAL USER REQUIREMENT"));
    assert!(digest.contains("Never commit without approval"));
    assert!(!digest.contains(ARCHIVE_NOTICE));
    assert!(!digest.contains("TEXT BEYOND CAP"));
    assert!(!digest.contains("ARGS BEYOND CAP"));
    assert!(
        digest.chars().count()
            <= DIGEST_TEXT_CHARS
                + PROGRESS_START.len()
                + PROGRESS_END.len()
                + "Never commit without approval".len()
    );
    assert!(
        digest
            .lines()
            .any(|line| line.starts_with("tool call: read_file(") && line.ends_with(')'))
    );
    assert_eq!(
        mechanical_digest(
            &[saved_checkpoint(
                "<compacted-summary>old</compacted-summary>"
            )],
            None
        ),
        "old"
    );
}

#[tokio::test]
async fn failed_recompaction_preserves_checkpoint_constraints() {
    let config = AppConfig {
        base_url: "invalid://no-summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let mut messages = assemble(
        &[
            system(),
            user("earlier"),
            assistant("done"),
            user("continue"),
        ],
        3,
        "Never commit without approval; preserve the old file format.",
    );
    messages.push(assistant(&"recent progress ".repeat(2_000)));
    messages.push(user("finish"));
    for _ in 0..2 {
        let cut = messages.len() - 1;
        let next = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            cut,
            None,
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        let checkpoint = next.messages[1]["content"].as_str().unwrap();
        assert!(checkpoint.contains("Never commit without approval"));
        assert!(checkpoint.contains("preserve the old file format"));
        assert!(checkpoint.contains("finish"));
        assert!(!checkpoint.contains(DROPPED));
        messages = next.messages;
        messages.push(assistant(&"more progress ".repeat(2_000)));
        messages.push(user("finish"));
    }
}

#[tokio::test]
async fn fallback_keeps_long_checkpoint_constraints_across_repeated_failures() {
    let summary = format!(
        "## Goal\n{}\n## Decisions and constraints\nNever commit without approval\n## Remaining work\nPreserve the old file format",
        "progress ".repeat(700)
    );
    assert!(summary.split_whitespace().count() < 800);
    assert!(estimate_text(&summary) < 4_000);
    let config = AppConfig {
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let mut messages = assemble(
        &[system(), user("old"), assistant("done"), user("finish")],
        3,
        &summary,
    );
    for _ in 0..3 {
        messages.push(assistant(&"recent progress ".repeat(2_000)));
        messages.push(user("finish"));
        let cut = messages.len() - 1;
        let next = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            cut,
            None,
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        let checkpoint = next.messages[1]["content"].as_str().unwrap();
        assert!(checkpoint.contains(&summary));
        assert!(checkpoint.ends_with("finish"));
        assert!(!checkpoint.contains(DROPPED));
        assert!(estimate_tokens(&next.messages) < estimate_tokens(&messages));
        messages = next.messages;
    }
}

#[tokio::test]
async fn long_checkpoint_keeps_new_progress_without_growing_on_every_fallback() {
    let summary = format!("{}\nNever commit without approval", "progress ".repeat(700));
    let mut messages = assemble(
        &[system(), user("old"), assistant("done"), user("finish")],
        3,
        &summary,
    );
    let config = AppConfig {
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    let mut sizes = Vec::new();
    for step in 0..6 {
        messages.push(assistant(&format!(
            "Migration {step} completed; do not run it again."
        )));
        messages.push(tool(
            "latest",
            &format!("LATEST_ERROR_{step}: deployment verification failed"),
        ));
        messages.push(assistant(&"old log ".repeat(2000)));
        messages.push(user("finish"));
        let cut = messages.len() - 1;
        let next = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            cut,
            None,
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        let text = next.messages[1]["content"].as_str().unwrap();
        assert!(text.contains(&summary));
        assert!(text.contains(&format!("Migration {step} completed")));
        assert!(text.contains(&format!("LATEST_ERROR_{step}")));
        assert!(text.ends_with("finish"));
        assert_eq!(text.matches(PROGRESS_START).count(), 1);
        assert!(text.chars().count() < summary.chars().count() + DIGEST_TEXT_CHARS + 500);
        sizes.push(text.len());
        messages = next.messages;
    }
    assert_eq!(sizes[1..].iter().min(), sizes[1..].iter().max());
}

#[tokio::test]
async fn new_user_constraints_survive_repeated_fallbacks_and_large_logs() {
    let base = saved_checkpoint(
        "<compacted-summary>Goal: finish migration.</compacted-summary>\n\ncontinue",
    );
    let constraint = "Use staging only; never deploy to production.\n保留旧格式，禁止提交。";
    let mut messages = vec![
        system(),
        base,
        user(constraint),
        assistant(&"old log ".repeat(2_000)),
        assistant("Latest verification failed."),
        user("finish"),
    ];
    let config = AppConfig {
        base_url: "invalid://summary".into(),
        ..AppConfig::default()
    };
    let (_handle, cancel) = crate::tools::CancelHandle::new();
    for step in 0..6 {
        let cut = messages.len() - 1;
        let next = compact_at(
            &reqwest::Client::new(),
            &config,
            &messages,
            cut,
            None,
            &cancel,
            CompactMode::Auto,
        )
        .await
        .unwrap();
        let text = next.messages[1]["content"].as_str().unwrap();
        let summary = checkpoint_parts(text).0;
        let encoded = summary
            .split_once(REQUESTS_START)
            .unwrap()
            .1
            .split_once(REQUESTS_END)
            .unwrap()
            .0;
        let requests: Vec<String> = serde_json::from_str(encoded).unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|text| text.as_str() == constraint)
                .count(),
            1
        );
        assert!(text.contains("Goal: finish migration."));
        assert!(text.chars().count() < DIGEST_TEXT_CHARS + 1000);
        if step == 1 {
            assert!(text.contains("Latest verification failed."));
        }
        messages = next.messages;
        if step > 0 {
            messages.push(assistant(&"new log ".repeat(2000)));
        }
        messages.push(user("finish"));
    }
}

#[test]
fn legacy_progress_user_constraints_are_preserved() {
    let prior = format!(
        "Goal{PROGRESS_START}user: staging only; never deploy\n\nAlso preserve the old file format.\n\nassistant: verified{PROGRESS_END}"
    );
    let summary = mechanical_digest(
        &[
            saved_checkpoint(&format!("<compacted-summary>{prior}</compacted-summary>")),
            assistant("still working"),
        ],
        None,
    );
    assert!(summary.contains("staging only; never deploy"));
    assert!(summary.contains("Also preserve the old file format."));
    assert!(summary.contains("verified"));
    assert!(summary.contains("still working"));
}

#[test]
fn fallback_progress_keeps_the_newest_entries() {
    let prior = format!(
        "Goal{PROGRESS_START}{}{PROGRESS_END}",
        "old progress ".repeat(150)
    );
    let mut messages = vec![
        system(),
        saved_checkpoint(&format!("<compacted-summary>{prior}</compacted-summary>")),
    ];
    for index in 0..3 {
        messages.push(assistant(&format!("entry {index} {}", "x".repeat(2_000))));
    }
    messages.push(assistant("NEWEST_ENTRY"));
    let summary = mechanical_digest(&messages[1..], None);
    assert!(summary.contains("NEWEST_ENTRY"));
    assert!(summary.contains("old progress"));
}

#[test]
fn repeated_fallbacks_keep_the_request_block_bounded() {
    let mut prior = "Goal: ship it.".to_string();
    let mut sizes = Vec::new();
    for index in 0..8 {
        let messages = [
            system(),
            saved_checkpoint(&format!("<compacted-summary>{prior}</compacted-summary>")),
            user(&format!("request {index}: {}", "a".repeat(3_000))),
            assistant("done"),
        ];
        let digest = mechanical_digest(&messages[1..], None);
        let encoded = digest
            .split_once(REQUESTS_START)
            .unwrap()
            .1
            .split_once(REQUESTS_END)
            .unwrap()
            .0;
        let requests: Vec<String> = serde_json::from_str(encoded).unwrap();
        assert!(requests.len() <= 3);
        assert!(requests[0].starts_with("request 0:"));
        assert!(
            requests
                .last()
                .unwrap()
                .starts_with(&format!("request {index}:"))
        );
        assert!(digest.contains("Goal: ship it."));
        sizes.push(digest.chars().count());
        prior = digest;
    }
    assert!(sizes.iter().all(|size| *size < 8_000), "{sizes:?}");
}

#[test]
fn mechanical_fallback_drops_only_when_the_checkpoint_would_not_shrink() {
    let messages = vec![system(), user("hi"), assistant("hello"), user("continue")];
    assert_eq!(fallback_summary(&messages, 3, None), DROPPED);
}

#[test]
fn fallback_does_not_keep_a_prior_checkpoint_that_would_grow() {
    let checkpoint = saved_checkpoint(&format!(
        "<compacted-summary>{}</compacted-summary>",
        "x".repeat(20_000)
    ));
    let messages = vec![system(), checkpoint, assistant("tail")];
    assert_eq!(fallback_summary(&messages, 2, None), DROPPED);
}

#[test]
fn merges_the_checkpoint_into_a_leading_user_turn() {
    let messages = vec![system(), user("old"), assistant("working"), user("new")];
    let next = assemble(&messages, 3, "checkpoint");
    assert_eq!(next.len(), 2);
    assert_eq!(next[0]["role"], "system");
    let content = next[1]["content"].as_str().unwrap();
    assert!(content.contains("checkpoint"));
    assert!(content.contains("new"));
}

#[test]
fn inserts_the_checkpoint_before_an_assistant_tail() {
    let messages = vec![system(), user("old"), assistant("kept")];
    let next = assemble(&messages, 2, "checkpoint");
    assert_eq!(next.len(), 3);
    assert_eq!(next[1]["role"], "user");
    assert!(next[1]["content"].as_str().unwrap().contains("checkpoint"));
    assert_eq!(next[2]["content"], "kept");
}

#[test]
fn skipping_the_live_request_still_preserves_its_checkpoint() {
    let messages = [
        saved_checkpoint(
            "<compacted-summary>Never deploy to production.</compacted-summary>\n\nLIVE_REQUEST",
        ),
        assistant("Completed the migration."),
    ];
    let digest = mechanical_digest(&messages, Some(0));
    assert!(digest.contains("Never deploy to production."));
    assert!(digest.contains("Completed the migration."));
    assert!(!digest.contains("LIVE_REQUEST"));
}

#[test]
fn merged_user_quotes_are_not_parsed_as_additional_checkpoints() {
    let request =
        "<compacted-summary>literal user example</compacted-summary>\n\nPlease explain this log.";
    let compacted = assemble(
        &[system(), user("old"), assistant("done"), user(request)],
        3,
        "actual summary",
    );
    let (summary, plain) = checkpoint_parts(compacted[1]["content"].as_str().unwrap());
    assert_eq!(summary, "actual summary");
    assert_eq!(plain, request);
    let digest = mechanical_digest(&compacted[1..], None);
    assert!(digest.contains("literal user example"));
    assert!(digest.contains("Please explain this log."));
}
