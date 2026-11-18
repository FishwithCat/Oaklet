//! Test-only LSP probe. See docs/plan/code-index-spike.md for invocations.
#[path = "lsp_spike/client.rs"]
mod client;

use std::path::Path;
use std::time::{Duration, Instant};

use client::{Client, Result};
use lsp_types::{notification, request};
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(30);

async fn probe(
    client: &Client,
    root: &Path,
    language: &str,
    file: &Path,
    symbol: &str,
    config: Value,
) -> Result<Value> {
    let started = Instant::now();
    let initialized = client.initialize(root, config).await?;
    let text = std::fs::read_to_string(file)?;
    let (line, source) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(symbol))
        .ok_or("Symbol not found")?;
    let column = source[..source.find(symbol).ok_or("Symbol not found")?]
        .encode_utf16()
        .count();
    let uri = lsp_types::Url::from_file_path(file).map_err(|_| "Invalid file")?;
    client.server.notify::<notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: language.into(),
                version: 1,
                text,
            },
        },
    )?;
    let position =
        json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": column}});
    let mut last = String::new();
    let readiness = Instant::now();
    let definition = loop {
        match client
            .request::<request::GotoDefinition>(position.clone(), DEADLINE)
            .await
        {
            Ok(value) if !value.is_null() && value != json!([]) => break value,
            Ok(_) => last = "Empty definition while loading".into(),
            Err(error)
                if matches!(error.downcast_ref::<async_lsp::Error>(),
                Some(async_lsp::Error::Response(response)) if [async_lsp::ErrorCode::CONTENT_MODIFIED,
                    async_lsp::ErrorCode::SERVER_NOT_INITIALIZED, async_lsp::ErrorCode::REQUEST_CANCELLED].contains(&response.code)) =>
            {
                last = error.to_string()
            }
            Err(error) => return Err(error),
        }
        if readiness.elapsed() > Duration::from_secs(120) {
            return Err(format!("Definition never became available: {last}").into());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let mut references_params = position.clone();
    references_params["context"] = json!({"includeDeclaration": false});
    let mut references = client
        .request::<request::References>(references_params.clone(), DEADLINE)
        .await?;
    let mut reference_samples = vec![
        json!({"elapsedMs": started.elapsed().as_millis(), "count": references.as_array().map(Vec::len)}),
    ];
    let symbols = client
        .request::<request::DocumentSymbolRequest>(json!({"textDocument": {"uri": uri}}), DEADLINE)
        .await;
    let capabilities = &initialized["capabilities"];
    let mut calls = json!({"status": "unsupported"});
    if capabilities["callHierarchyProvider"] == true
        || capabilities["callHierarchyProvider"].is_object()
    {
        match client
            .request::<request::CallHierarchyPrepare>(position, DEADLINE)
            .await
        {
            Ok(items) if items.as_array().is_some_and(|items| !items.is_empty()) => {
                let item = &items[0];
                calls = json!({"status": "ok", "candidateCount": items.as_array().map(Vec::len),
                    "incoming": outcome(client.request::<request::CallHierarchyIncomingCalls>(json!({"item": item}), DEADLINE).await),
                    "outgoing": outcome(client.request::<request::CallHierarchyOutgoingCalls>(json!({"item": item}), DEADLINE).await)});
            }
            Ok(items) => calls = json!({"status": "empty", "items": items}),
            Err(error) => calls = json!({"status": "error", "error": error.to_string()}),
        }
    }
    // Observe startup drift; repeated results are not proof of whole-project completeness.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        references = client
            .request::<request::References>(references_params.clone(), DEADLINE)
            .await?;
        reference_samples.push(json!({"elapsedMs": started.elapsed().as_millis(), "count": references.as_array().map(Vec::len)}));
    }
    if references.as_array().is_none_or(Vec::is_empty) {
        return Err("References were empty; probe not accepted".into());
    }
    Ok(
        json!({"root": root, "file": file, "symbol": symbol, "line": line + 1, "column": column + 1,
        "elapsedMs": started.elapsed().as_millis(), "serverInfo": initialized["serverInfo"],
        "capabilities": capabilities, "definition": definition, "references": references, "referenceSamples": reference_samples,
        "symbols": outcome(symbols), "calls": calls, "readinessLastError": last}),
    )
}

fn outcome(result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"status": "ok", "result": value}),
        Err(error) => json!({"status": "error", "error": error.to_string()}),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        return Err("Usage: lsp_spike ROOT LANGUAGE FILE SYMBOL SERVER [SERVER_ARGS...]".into());
    }
    let root = Path::new(&args[0]).canonicalize()?;
    let file = root.join(&args[2]).canonicalize()?;
    if !file.starts_with(&root) || args[3].is_empty() {
        return Err("File must be inside ROOT and SYMBOL must be nonempty".into());
    }
    let config = if args[1] == "rust" {
        json!({"cargo": {"buildScripts": {"enable": false}, "targetDir": "target/oaklet-lsp-spike"},
            "procMacro": {"enable": false}, "checkOnSave": false})
    } else {
        json!({})
    };
    let mut client = Client::spawn(&root, &args[4], &args[5..], config.clone())?;
    let report = probe(&client, &root, &args[1], &file, &args[3], config).await;
    let cleanup = client.finish().await?;
    let passed = report.is_ok() && cleanup["shutdown"] == "ok" && cleanup["exitCode"] == 0;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"probe": outcome(report), "cleanup": cleanup}))?
    );
    if !passed {
        return Err("LSP probe failed; see JSON report".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "lsp_spike/tests.rs"]
mod tests;
