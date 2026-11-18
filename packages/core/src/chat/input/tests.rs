use super::*;

#[test]
fn reads_the_composer_attachment_shape() {
    let image: IncomingAttachment = serde_json::from_value(json!({
        "kind": "image",
        "name": "shot.png",
        "bytes": 10,
        "dataUrl": "data:image/png;base64,AAA",
        "thumbnail": "data:image/jpeg;base64,BBB",
    }))
    .unwrap();
    match image {
        IncomingAttachment::Image { data_url, .. } => {
            assert_eq!(data_url, "data:image/png;base64,AAA");
        }
        _ => panic!("expected an image"),
    }

    let file: IncomingAttachment = serde_json::from_value(json!({
        "kind": "file",
        "name": "notes.md",
        "bytes": 3,
        "text": "hi",
    }))
    .unwrap();
    match file {
        IncomingAttachment::File { text, .. } => assert_eq!(text, "hi"),
        _ => panic!("expected a file"),
    }
}

#[test]
fn builds_user_content_and_display_items_for_attachments() {
    let (message, display, names) = build_user_turn(
        "look",
        &[
            IncomingAttachment::Image {
                name: "shot.png".into(),
                bytes: 10,
                data_url: "data:image/png;base64,AAA".into(),
                thumbnail: "data:image/jpeg;base64,BBB".into(),
            },
            IncomingAttachment::File {
                name: "notes.md".into(),
                bytes: 3,
                text: "hi".into(),
            },
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "run-1",
    )
    .unwrap();

    assert_eq!(names, vec!["shot.png", "notes.md"]);
    assert_eq!(
        message["content"][0],
        json!({ "type": "text", "text": "look" })
    );
    assert_eq!(
        message["content"][1],
        json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } })
    );
    assert_eq!(
        message["content"][2],
        json!({ "type": "text", "text": "Attached file notes.md:\n\nhi" })
    );
    assert_eq!(display["text"], "look");
    assert_eq!(
        display["attachments"][0]["thumbnail"],
        "data:image/jpeg;base64,BBB"
    );
    assert_eq!(display["attachments"][1]["name"], "notes.md");
}

#[test]
fn plain_text_keeps_the_string_content_shape() {
    let (message, display, names) =
        build_user_turn("hello", &[], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").unwrap();
    assert_eq!(message["content"], "hello");
    assert_eq!(display["text"], "hello");
    assert!(names.is_empty());
    assert!(build_user_turn("", &[], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").is_err());
}

#[test]
fn rejects_oversized_attachments() {
    let file = IncomingAttachment::File {
        name: "big.txt".into(),
        bytes: 1,
        text: "x".repeat(200_001),
    };
    assert!(build_user_turn("", &[file], Path::new(env!("CARGO_MANIFEST_DIR")), "run-1").is_err());
}

#[test]
fn spills_oversized_text_attachments_with_a_preview() {
    let spill_root = crate::tools::test_support::temp_root("attachment-spill");
    let head: Vec<String> = (1..=100).map(|line| format!("line {line}")).collect();
    let tail: Vec<String> = (500..=540).map(|line| format!("line {line}")).collect();
    let text = format!(
        "{}\n{}\n{}",
        head.join("\n"),
        "x".repeat(ATTACHMENT_SPILL_BYTES),
        tail.join("\n")
    );
    let file = IncomingAttachment::File {
        name: "notes.md".into(),
        bytes: text.len() as u64,
        text: text.clone(),
    };
    let (message, _display, names) = build_user_turn("", &[file], &spill_root, "run-1").unwrap();

    let directory = std::fs::read_dir(&spill_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let path = directory.join("1-notes.md.txt");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "spill file keeps the complete attachment"
    );
    let body = message["content"][0]["text"].as_str().unwrap();
    assert_eq!(names, vec!["notes.md"]);
    assert!(body.contains(&path.display().to_string()));
    assert!(body.contains(&format!("{} lines", text.lines().count())));
    assert!(body.contains(&format!("{} bytes", text.len())));
    assert!(body.contains("line 1"));
    assert!(body.contains("line 540"));
    assert!(body.contains("lines omitted"));
    assert!(!body.contains(&"x".repeat(1_000)));
    assert!(body.chars().count() < 10_000);
    std::fs::remove_dir_all(&spill_root).ok();
}

#[test]
fn attachment_spill_failure_keeps_the_inline_text() {
    let root = crate::tools::test_support::temp_root("attachment-spill-fail");
    let spill_root = root.join("spill");
    std::fs::write(&spill_root, "not a directory").unwrap();
    let text = "x".repeat(ATTACHMENT_SPILL_BYTES + 1);
    let file = IncomingAttachment::File {
        name: "notes.txt".into(),
        bytes: text.len() as u64,
        text: text.clone(),
    };
    let (message, _item, _names) = build_user_turn("", &[file], &spill_root, "run-1").unwrap();

    assert_eq!(
        message["content"][0]["text"],
        format!("Attached file notes.txt:\n\n{text}")
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn derives_titles_from_text_or_attachment_names() {
    assert_eq!(
        session_title("  Fix\n the   parser ", &[]),
        "Fix the parser"
    );
    assert_eq!(
        session_title("", &["error.log".into(), "notes.md".into()]),
        "error.log, notes.md"
    );
    let long = "x".repeat(80);
    assert_eq!(session_title(&long, &[]).chars().count(), 60);
    let cjk = "你".repeat(80);
    assert_eq!(session_title(&cjk, &[]).chars().count(), 60);
    assert_eq!(session_title("", &[]), "");
}
