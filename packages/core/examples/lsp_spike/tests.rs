use super::*;
use tokio::io::AsyncReadExt;

fn fake() -> Client {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Client::spawn(
        root,
        "python3",
        &[root
            .join("examples/lsp_spike/fake_server.py")
            .to_string_lossy()
            .into_owned()],
        json!({"test": true}),
    )
    .unwrap()
}

#[tokio::test]
async fn handles_server_requests_notifications_timeout_and_late_response() {
    let mut client = fake();
    client
        .initialize(Path::new(env!("CARGO_MANIFEST_DIR")), json!({}))
        .await
        .unwrap();
    assert_eq!(
        client
            .notifications
            .lock()
            .unwrap()
            .get("window/logMessage"),
        Some(&1)
    );
    let params = json!({"textDocument": {"uri": "file:///spike.rs"}, "position": {"line": 0, "character": 0}});
    let error = client
        .request::<request::GotoDefinition>(params, Duration::from_millis(30))
        .await
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
    );
    let result = client
        .request::<request::WorkspaceSymbolRequest>(json!({"query": "probe"}), DEADLINE)
        .await
        .unwrap();
    // The vendored cancellation fix also applies to the spike.
    assert_eq!(result[0]["name"], "cancelled");
    let error = client
        .request::<request::DocumentSymbolRequest>(
            json!({"textDocument": {"uri": "file:///spike.rs"}}),
            DEADLINE,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<async_lsp::Error>(), Some(async_lsp::Error::Response(error)) if error.code == async_lsp::ErrorCode::METHOD_NOT_FOUND)
    );
    let cleanup = client.finish().await.unwrap();
    assert_eq!(cleanup["shutdown"], "ok");
    assert_eq!(cleanup["exitCode"], 0);
}

#[tokio::test]
async fn malformed_json_stops_protocol_instead_of_returning_empty() {
    let mut client = fake();
    client
        .initialize(Path::new(env!("CARGO_MANIFEST_DIR")), json!({}))
        .await
        .unwrap();
    let result = client
        .request::<request::References>(
            json!({"textDocument": {"uri": "file:///spike.rs"},
        "position": {"line": 0, "character": 0}, "context": {"includeDeclaration": false}}),
            Duration::from_secs(1),
        )
        .await;
    assert!(result.is_err());
    let cleanup = client.finish().await.unwrap();
    assert_eq!(cleanup["shutdown"], "failed");
    assert_eq!(cleanup["exitCode"], 0);
}

#[tokio::test]
async fn frame_guard_passes_multiple_frames_and_rejects_oversized_headers_bodies_and_truncation() {
    let valid = b"Content-Length: 2\r\n\r\n{}Content-Length: 4\r\n\r\nnull";
    let (mut read, write) = tokio::io::duplex(128);
    client::forward_bounded(&valid[..], write).await.unwrap();
    let mut actual = Vec::new();
    read.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, valid);
    let oversized = format!("Content-Length: {}\r\n\r\n", client::MAX_MESSAGE_BYTES + 1);
    let huge_header = vec![b'x'; 8193];
    for invalid in [
        oversized.as_bytes(),
        &huge_header,
        b"Content-Length: 5\r\n\r\n{}",
        b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx",
    ] {
        let error = client::forward_bounded(invalid, tokio::io::sink())
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty());
    }
}
