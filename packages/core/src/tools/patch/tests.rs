//! `apply_patch` parsing, preflight, commit, and cancellation behavior.

use serde_json::json;

use super::apply::apply_hunks;
use super::commit::{commit, commit_operations, execute_one};
use super::parse::{PatchOperation, parse_patch};
use super::plan::{plan, preflight};
use super::tool::apply_patch;
use crate::tools::ToolError;
use crate::tools::test_support::context;

fn run(ctx: &crate::tools::ToolContext, input: &str) -> Result<String, String> {
    apply_patch(ctx, json!({ "input": input }))
        .map(|output| output.text)
        .map_err(|error| error.to_string())
}

fn text(path: std::path::PathBuf) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn parses_ordered_operations_and_move_endpoints() {
    let input = "\
*** Begin Patch
*** Add File: a.txt
+hello
+
*** Update File: b.txt
*** Move to: c.txt
@@ section
-old
+new
 same
*** Delete File: d.txt
*** End Patch";
    let operations = parse_patch(input).unwrap();
    assert_eq!(operations.len(), 3);
    match &operations[0] {
        PatchOperation::Add {
            path,
            line,
            content,
        } => {
            assert_eq!(path, "a.txt");
            assert_eq!(*line, 2);
            assert_eq!(content, "hello\n\n");
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    match &operations[1] {
        PatchOperation::Update {
            path,
            line,
            move_to,
            hunks,
        } => {
            assert_eq!(path, "b.txt");
            assert_eq!(*line, 5);
            assert_eq!(move_to.as_deref(), Some("c.txt"));
            assert_eq!(hunks[0].context.as_deref(), Some("section"));
            assert_eq!(hunks[0].old, vec!["old", "same"]);
            assert_eq!(hunks[0].new, vec!["new", "same"]);
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    assert!(matches!(
        &operations[2],
        PatchOperation::Delete { path, line } if path == "d.txt" && *line == 11
    ));
}

#[test]
fn accepts_heredoc_wrappers_and_empty_patches() {
    let operations =
        parse_patch("\n<<'EOF'\n*** Begin Patch\n*** Add File: x\n+y\n*** End Patch\nEOF\n")
            .unwrap();
    assert_eq!(
        operations,
        vec![PatchOperation::Add {
            path: "x".into(),
            line: 4,
            content: "y\n".into(),
        }]
    );
    assert_eq!(
        parse_patch("*** Begin Patch\n*** End Patch").unwrap(),
        vec![]
    );
}

#[test]
fn rejects_malformed_input_at_its_line() {
    let error = parse_patch("not a patch").unwrap_err();
    assert_eq!(error.0, 1);
    assert!(error.1.contains("Begin Patch"));

    let error = parse_patch("*** Begin Patch\n*** Update File: a\n*** End Patch").unwrap_err();
    assert_eq!(error.0, 2);
    assert!(error.1.contains("requires at least one hunk"));

    let error =
        parse_patch("*** Begin Patch\n*** Update File: a\nunprefixed\n*** End Patch").unwrap_err();
    assert_eq!(error.0, 3);
    assert!(error.1.contains("context"));
}

#[test]
fn matches_hunks_with_whitespace_tolerance_and_eof_anchoring() {
    let operations = parse_patch(
        "*** Begin Patch\n*** Update File: a\n@@ two\n-three\n+THREE\n*** End of File\n*** End Patch",
    )
    .unwrap();
    let PatchOperation::Update { hunks, .. } = &operations[0] else {
        panic!("expected update");
    };
    assert!(hunks[0].eof);
    assert_eq!(
        apply_hunks("one\ntwo\nthree\n", hunks).unwrap(),
        "one\ntwo\nTHREE\n"
    );

    let operations =
        parse_patch("*** Begin Patch\n*** Update File: a\n@@\n-alpha\n+ALPHA\n*** End Patch")
            .unwrap();
    let PatchOperation::Update { hunks, .. } = &operations[0] else {
        panic!("expected update");
    };
    assert_eq!(
        apply_hunks("alpha   \nbeta\n", hunks).unwrap(),
        "ALPHA\nbeta\n"
    );
}

#[test]
fn applies_add_update_delete_and_move_together() {
    let (ctx, root, _handle) = context("patch-apply");
    std::fs::write(root.join("notes.txt"), "keep\nold\n").unwrap();
    std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
    std::fs::write(root.join("move.txt"), "old\n").unwrap();

    let output = run(
        &ctx,
        "\
*** Begin Patch
*** Add File: new.txt
+hello
*** Update File: notes.txt
@@ keep
-old
+new
*** Delete File: gone.txt
*** Update File: move.txt
*** Move to: moved.txt
@@
-old
+new
*** End Patch",
    )
    .unwrap();

    assert_eq!(
        output,
        "Success. Updated the following files:\nA new.txt\nM notes.txt\nM moved.txt\nD gone.txt"
    );
    assert_eq!(text(root.join("new.txt")), "hello\n");
    assert_eq!(text(root.join("notes.txt")), "keep\nnew\n");
    assert!(!root.join("gone.txt").exists());
    assert_eq!(text(root.join("moved.txt")), "new\n");
    assert!(!root.join("move.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reports_applied_diffs_for_each_operation() {
    let (ctx, root, _handle) = context("patch-diffs");
    std::fs::write(root.join("notes.txt"), "keep\nold\n").unwrap();
    std::fs::write(root.join("gone.txt"), "bye\n").unwrap();

    let output = apply_patch(
        &ctx,
        json!({ "input": "\
*** Begin Patch
*** Add File: new.txt
+hello
*** Update File: notes.txt
@@ keep
-old
+new
*** Delete File: gone.txt
*** End Patch" }),
    )
    .unwrap();

    assert_eq!(output.diffs.len(), 3);
    assert_eq!(output.diffs[0].path, "new.txt");
    assert_eq!(output.diffs[0].old_text, None);
    assert_eq!(output.diffs[0].new_text, "hello");
    assert_eq!(output.diffs[1].path, "notes.txt");
    assert_eq!(output.diffs[1].old_text.as_deref(), Some("keep\nold"));
    assert_eq!(output.diffs[1].new_text, "keep\nnew");
    assert_eq!(output.diffs[2].path, "gone.txt");
    assert_eq!(output.diffs[2].old_text.as_deref(), Some("bye"));
    assert_eq!(output.diffs[2].new_text, "");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn keeps_crlf_line_endings_when_patching() {
    let (ctx, root, _handle) = context("patch-crlf");
    std::fs::write(root.join("a.txt"), "one\r\ntwo\r\n").unwrap();

    run(
        &ctx,
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n*** End Patch",
    )
    .unwrap();

    assert_eq!(text(root.join("a.txt")), "ONE\r\ntwo\r\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn keeps_crlf_without_a_trailing_newline() {
    let (ctx, root, _handle) = context("patch-crlf-tail");
    std::fs::write(root.join("a.txt"), "one\r\ntwo").unwrap();

    run(
        &ctx,
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n*** End Patch",
    )
    .unwrap();

    assert_eq!(text(root.join("a.txt")), "ONE\r\ntwo\r\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn mixed_line_endings_stay_lf_after_patching() {
    let (ctx, root, _handle) = context("patch-mixed-endings");
    std::fs::write(root.join("a.txt"), "one\r\ntwo\n").unwrap();

    run(
        &ctx,
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n*** End Patch",
    )
    .unwrap();

    assert_eq!(text(root.join("a.txt")), "ONE\ntwo\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn rejects_the_whole_patch_before_writing() {
    let (ctx, root, _handle) = context("patch-reject");
    std::fs::write(root.join("a.txt"), "actual\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();

    let error = run(
        &ctx,
        "\
*** Begin Patch
*** Update File: a.txt
@@
-actual
+changed
*** Update File: b.txt
@@
-missing
+changed
*** End Patch",
    )
    .unwrap_err();

    assert!(error.contains("Patch rejected before any change (1 problems)"));
    assert!(error.contains("b.txt"));
    assert_eq!(text(root.join("a.txt")), "actual\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_stopped_patch_is_a_failure_even_after_writing_earlier_operations() {
    let (ctx, root, _handle) = context("patch-partial");
    let operations = parse_patch(
        "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
    )
    .unwrap();
    let planned = plan(&ctx, operations).unwrap();
    preflight(&planned, &ctx.cancel).unwrap();
    // The preflight passed, so the only remaining failure is a commit-time
    // race. Simulate one by creating the second target before commit.
    std::fs::write(root.join("second.txt"), "raced\n").unwrap();

    let error = commit(&planned, &ctx.cancel).unwrap_err().to_string();
    assert!(error.starts_with("Patch stopped after 1 of 2 operations."));
    assert!(error.contains("Completed:\nA first.txt"));
    assert!(error.contains("Failed:\n- operation 2 (add second.txt"));
    assert_eq!(text(root.join("first.txt")), "first\n");
    assert_eq!(text(root.join("second.txt")), "raced\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn cancellation_before_commit_writes_nothing() {
    let (ctx, root, handle) = context("patch-cancel-before");
    let operations = parse_patch(
        "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
    )
    .unwrap();
    let planned = plan(&ctx, operations).unwrap();
    handle.cancel();

    assert!(matches!(
        preflight(&planned, &ctx.cancel),
        Err(ToolError::Cancelled)
    ));
    assert!(matches!(
        commit(&planned, &ctx.cancel),
        Err(ToolError::Cancelled)
    ));
    let error = run(
        &ctx,
        "*** Begin Patch\n*** Add File: first.txt\n+first\n*** End Patch",
    )
    .unwrap_err();
    assert_eq!(error, "Cancelled.");
    assert!(!root.join("first.txt").exists());
    assert!(!root.join("second.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn cancellation_stops_before_the_next_operation() {
    let (ctx, root, handle) = context("patch-cancel-between");
    let operations = parse_patch(
        "*** Begin Patch\n*** Add File: first.txt\n+first\n*** Add File: second.txt\n+second\n*** End Patch",
    )
    .unwrap();
    let planned = plan(&ctx, operations).unwrap();
    preflight(&planned, &ctx.cancel).unwrap();

    let mut first = true;
    let error = commit_operations(&planned, &ctx.cancel, |operation| {
        let result = execute_one(operation);
        if first {
            first = false;
            handle.cancel();
        }
        result
    })
    .unwrap_err()
    .to_string();

    assert!(error.starts_with("Patch cancelled after 1 of 2 operations."));
    assert!(error.contains("Completed:\nA first.txt"));
    assert!(error.contains("Not executed:\n- operation 2 (add second.txt)"));
    assert_eq!(text(root.join("first.txt")), "first\n");
    assert!(!root.join("second.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn refuses_to_add_an_existing_file() {
    let (ctx, root, _handle) = context("patch-add");
    std::fs::write(root.join("a.txt"), "old\n").unwrap();

    let error = run(
        &ctx,
        "*** Begin Patch\n*** Add File: a.txt\n+new\n*** End Patch",
    )
    .unwrap_err();
    assert!(error.contains("file already exists"));
    assert_eq!(text(root.join("a.txt")), "old\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn refuses_comments_added_by_a_patch() {
    let (ctx, root, _handle) = context("patch-comments");
    let error = run(
        &ctx,
        "*** Begin Patch\n*** Add File: src/main.rs\n+// entry point\n+fn main() {}\n*** End Patch",
    )
    .unwrap_err();
    assert!(error.contains("src/main.rs"));
    assert!(error.contains("New code comments are not allowed"));
    assert!(!root.join("src/main.rs").exists());

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "// kept\npub fn value() -> u8 { 1 }\n",
    )
    .unwrap();
    std::fs::write(root.join("other.txt"), "before\n").unwrap();
    let error = run(
        &ctx,
        "\
*** Begin Patch
*** Update File: other.txt
@@
-before
+after
*** Update File: src/lib.rs
@@
+// explain
 pub fn value() -> u8 { 1 }
*** End Patch",
    )
    .unwrap_err();
    assert!(
        error.contains("Patch rejected before any change"),
        "{error}"
    );
    assert!(error.contains("line 2: // explain"), "{error}");
    assert_eq!(text(root.join("other.txt")), "before\n");
    assert_eq!(
        text(root.join("src/lib.rs")),
        "// kept\npub fn value() -> u8 { 1 }\n"
    );

    run(
        &ctx,
        "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-pub fn value() -> u8 { 1 }\n+pub fn value() -> u8 { 2 }\n*** End Patch",
    )
    .unwrap();
    assert_eq!(
        text(root.join("src/lib.rs")),
        "// kept\npub fn value() -> u8 { 2 }\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn keeps_comments_when_a_patch_moves_a_file() {
    let (ctx, root, _handle) = context("patch-move-comments");
    std::fs::write(root.join("a.rs"), "// kept\nfn run() {}\n").unwrap();

    run(
        &ctx,
        "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-fn run() {}\n+fn run() { stop(); }\n*** End Patch",
    )
    .unwrap();
    assert!(!root.join("a.rs").exists());
    assert_eq!(text(root.join("b.rs")), "// kept\nfn run() { stop(); }\n");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn empty_patch_is_a_no_op() {
    let (ctx, root, _handle) = context("patch-empty");
    assert_eq!(
        run(&ctx, "*** Begin Patch\n*** End Patch").unwrap(),
        "Success. No operations in patch."
    );
    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[test]
fn refuses_to_delete_a_symlink_source() {
    let (ctx, root, _handle) = context("patch-symlink");
    std::fs::write(root.join("target.txt"), "keep\n").unwrap();
    std::os::unix::fs::symlink("target.txt", root.join("link")).unwrap();

    let error = run(
        &ctx,
        "*** Begin Patch\n*** Delete File: link\n*** End Patch",
    )
    .unwrap_err();

    assert!(error.contains("symbolic link"));
    assert!(root.join("link").exists());
    assert_eq!(text(root.join("target.txt")), "keep\n");
    std::fs::remove_dir_all(&root).ok();
}
#[test]
fn syntax_warnings_describe_the_final_patch_state_in_each_file() {
    let (ctx, root, _cancel) = crate::tools::test_support::context("patch-syntax");
    let patch = concat!(
        "*** Begin Patch\n",
        "*** Add File: fixed.ts\n+const x = (1;\n",
        "*** Update File: fixed.ts\n@@\n-const x = (1;\n+const x = 1;\n",
        "*** Add File: first.ts\n+const y = (2;\n",
        "*** Update File: first.ts\n@@\n const y = (2;\n+const tail = 0;\n",
        "*** Add File: second.rs\n+fn second() {\n",
        "*** Add File: third.py\n+# kept\n+def third():\n+    return (2\n",
        "*** Add File: Fourth.java\n+// kept\n+class Fourth {\n",
        "*** End Patch",
    );
    let result = super::tool::apply_patch(&ctx, serde_json::json!({ "input": patch })).unwrap();
    assert!(result.text.contains("Possible syntax errors"));
    assert!(result.text.contains("first.ts:1:"), "{}", result.text);
    assert!(result.text.contains("second.rs:1:"), "{}", result.text);
    assert!(result.text.contains("third.py:3:"), "{}", result.text);
    assert!(result.text.contains("Fourth.java:2:"), "{}", result.text);
    assert!(!result.text.contains("fixed.ts:"), "{}", result.text);
    assert_eq!(
        std::fs::read_to_string(root.join("fixed.ts")).unwrap(),
        "const x = 1;\n"
    );
    let _ = std::fs::remove_dir_all(root);
}
