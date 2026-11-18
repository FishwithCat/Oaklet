//! Inspect project discovery and server availability without starting a language server.
use std::path::Path;

use oaklet_lib::code_index::{CodeIndexConfig, detect_server, discover};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = args
        .first()
        .ok_or("Usage: code_projects WORKSPACE [FILE...]")?;
    let discovery = discover(Path::new(root)).await?;
    let mut servers = Vec::new();
    for project in &discovery.projects {
        let status = match detect_server(project, &CodeIndexConfig::default()).await {
            Ok(Some(command)) => json!({"status": "available", "command": command}),
            Ok(None) => json!({"status": "disabled"}),
            Err(error) => json!({"status": "unavailable", "error": error}),
        };
        servers
            .push(json!({"root": project.root, "language": project.language, "backend": status}));
    }
    let routes: Vec<_> = args[1..]
        .iter()
        .map(|file| match discovery.route(file) {
            Ok(project) => json!({"file": file, "project": project}),
            Err(error) => json!({"file": file, "error": error}),
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"discovery": discovery, "servers": servers, "routes": routes})
        )?
    );
    Ok(())
}
