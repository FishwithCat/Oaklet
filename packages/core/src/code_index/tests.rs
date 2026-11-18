use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::tools::test_support::temp_root;

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        Self(temp_root(tag))
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn krate(&self, root: &str, name: &str) {
        self.write(
            &format!("{root}/Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        );
        self.write(&format!("{root}/src/lib.rs"), "pub fn hello() {}\n");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn old_config_defaults_and_new_config_roundtrips_without_losing_server_settings() {
    let config: crate::AppConfig = serde_json::from_value(json!({"model": "m"})).unwrap();
    assert!(config.code_index.servers.is_empty());
    let mut config: crate::AppConfig = serde_json::from_value(json!({"codeIndex": {"servers": {
        "rust": {"command": " /custom/rust-analyzer ", "args": ["--log-file", "file with spaces"]},
        "typescript": {"enabled": false}
    }}}))
    .unwrap();
    config.code_index.validate().unwrap();
    let serialized = serde_json::to_value(&config).unwrap();
    assert_eq!(
        serialized["codeIndex"]["servers"]["rust"]["command"],
        "/custom/rust-analyzer"
    );
    assert_eq!(
        serialized["codeIndex"]["servers"]["rust"]["args"][1],
        "file with spaces"
    );
    let reread: crate::AppConfig = serde_json::from_value(serialized).unwrap();
    assert!(!reread.code_index.servers[&Language::TypeScript].enabled);
    assert!(reread.code_index.servers[&Language::Rust].enabled);
}

#[test]
fn validates_command_arguments_and_language_keys() {
    for raw in [
        json!({"command": " "}),
        json!({"command": "x\u{0}"}),
        json!({"args": ["arg"]}),
        json!({"command":"x", "args":["a\u{0}"]}),
    ] {
        let mut config = CodeIndexConfig {
            servers: BTreeMap::from([(Language::Rust, serde_json::from_value(raw).unwrap())]),
        };
        assert!(config.validate().is_err());
    }
    assert!(serde_json::from_value::<CodeIndexConfig>(json!({"servers":{"typo":{}}})).is_err());
}

#[tokio::test]
async fn discovers_cargo_workspace_independent_nested_crate_and_ts_packages() {
    let fixture = Fixture::new("code-projects");
    fixture.write("Cargo.toml", "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\nexclude = [\"nested\"]\nresolver = \"3\"\n");
    fixture.krate("crates/a", "a");
    fixture.krate("crates/b", "b");
    fixture.krate("nested", "nested");
    fixture.write("package.json", "{}");
    fixture.write("tsconfig.json", "{}");
    fixture.write("config/tsconfig.json", "{}");
    fixture.write("web/package.json", "{}");
    fixture.write("web/tsconfig.json", "{}");
    fixture.write("web/src/main.ts", "export const value = 1;");
    fixture.write("config/helper.js", "export const value = 2;");
    let result = discover(&fixture.0).await.unwrap();
    assert!(!result.incomplete, "{:?}", result.warnings);
    assert_eq!(result.projects.len(), 4, "{:?}", result.projects);
    assert_eq!(result.route("crates/a/src/lib.rs").unwrap().root, fixture.0);
    assert_eq!(result.route("crates/b/src/lib.rs").unwrap().root, fixture.0);
    assert_eq!(
        result.route("nested/src/lib.rs").unwrap().root,
        fixture.0.join("nested")
    );
    assert_eq!(
        result.route("web/src/main.ts").unwrap().root,
        fixture.0.join("web")
    );
    assert_eq!(result.route("config/helper.js").unwrap().root, fixture.0);
    assert_eq!(
        result.route("config/helper.js").unwrap().language,
        Language::TypeScript
    );
}

#[tokio::test]
async fn respects_ignore_rules_limits_and_missing_project_boundaries() {
    let fixture = Fixture::new("code-ignore");
    fixture.write(".gitignore", "ignored/\n");
    fixture.write(".ignore", "other/\n");
    for folder in ["app", "ignored", "other", "node_modules/pkg", "target/pkg"] {
        fixture.write(&format!("{folder}/tsconfig.json"), "{}");
        fixture.write(&format!("{folder}/main.ts"), "export const x = 1;");
    }
    fixture.write("loose.ts", "export const x = 1;");
    fixture.write("app/readme.md", "unsupported");
    let result = discover(&fixture.0).await.unwrap();
    assert_eq!(result.projects.len(), 1);
    assert!(result.route("app/main.ts").is_ok());
    std::fs::remove_file(fixture.0.join("app/main.ts")).unwrap();
    assert!(result.route("app/main.ts").is_err());
    for file in [
        "ignored/main.ts",
        "other/main.ts",
        "target/pkg/main.ts",
        "node_modules/pkg/main.ts",
        "app/readme.md",
        "missing.ts",
        "../outside.ts",
        "loose.ts",
    ] {
        assert!(result.route(file).is_err(), "{file}");
    }
    assert!(
        discovery::scan_limited(
            fixture.0.clone(),
            1,
            Instant::now() + Duration::from_secs(1)
        )
        .incomplete
    );
    assert!(discovery::scan_limited(fixture.0.clone(), 50_000, Instant::now()).incomplete);
}

#[tokio::test]
async fn broken_nested_cargo_manifest_does_not_fall_back_to_parent() {
    let fixture = Fixture::new("code-broken-cargo");
    fixture.krate("parent", "parent");
    fixture.write("parent/nested/Cargo.toml", "not valid TOML");
    fixture.write("parent/nested/src/lib.rs", "pub fn nested() {}");
    let result = discover(&fixture.0).await.unwrap();
    assert!(result.incomplete);
    assert!(result.route("parent/src/lib.rs").is_ok());
    assert!(
        result
            .route("parent/nested/src/lib.rs")
            .unwrap_err()
            .contains("failed")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn excludes_symlinked_projects_and_rejects_external_cargo_roots() {
    let fixture = Fixture::new("code-symlinks");
    let outside = Fixture::new("code-external");
    outside.write("tsconfig.json", "{}");
    outside.write("main.ts", "export const x = 1;");
    std::os::unix::fs::symlink(&outside.0, fixture.0.join("link")).unwrap();
    let result = discover(&fixture.0).await.unwrap();
    assert!(result.projects.is_empty());
    assert!(result.route("link/main.ts").is_err());
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"member\"]\nresolver = \"3\"\n",
    );
    fixture.krate("member", "member");
    let result = discover(&fixture.0.join("member")).await.unwrap();
    assert!(result.incomplete);
    assert!(result.route("src/lib.rs").unwrap_err().contains("outside"));
}

#[tokio::test]
async fn disabled_and_missing_servers_are_distinct() {
    let fixture = Fixture::new("code-server-disabled");
    let project = Project {
        root: fixture.0.clone(),
        language: Language::TypeScript,
        error: None,
    };
    let config = CodeIndexConfig {
        servers: BTreeMap::from([(
            Language::TypeScript,
            ServerConfig {
                enabled: false,
                ..ServerConfig::default()
            },
        )]),
    };
    assert!(
        runtime::detect_with_path(&project, &config, None)
            .await
            .unwrap()
            .is_none()
    );
    let error = runtime::detect_with_path(&project, &CodeIndexConfig::default(), None)
        .await
        .unwrap_err();
    assert!(error.contains("executable not found"));
}

#[cfg(unix)]
fn executable(fixture: &Fixture, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fixture.write(name, body);
    let path = fixture.0.join(name);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
async fn detects_explicit_server_sdk_and_missing_node_without_shell_interpolation() {
    let fixture = Fixture::new("code-server-runtime");
    let server = executable(
        &fixture,
        "bin/server",
        "#!/bin/sh\n[ \"$1\" = \"literal; argument\" ] || exit 2\n[ \"$2\" = \"--version\" ] || exit 3\nprintf 'server-1\\n'\n",
    );
    executable(&fixture, "bin/node", "#!/bin/sh\nprintf 'v26.0.0\\n'\n");
    let project = Project {
        root: fixture.0.clone(),
        language: Language::TypeScript,
        error: None,
    };
    let config = CodeIndexConfig {
        servers: BTreeMap::from([(
            Language::TypeScript,
            ServerConfig {
                command: Some(server.to_string_lossy().into_owned()),
                args: vec!["literal; argument".into()],
                enabled: true,
            },
        )]),
    };
    assert!(
        runtime::detect_with_path(&project, &config, None)
            .await
            .unwrap_err()
            .contains("node")
    );
    let path = fixture.0.join("bin").into_os_string();
    assert!(
        runtime::detect_with_path(&project, &config, Some(&path))
            .await
            .unwrap_err()
            .contains("TypeScript SDK")
    );
    fixture.write(
        "node_modules/typescript/package.json",
        r#"{"name":"typescript","version":"5.9.3"}"#,
    );
    fixture.write("node_modules/typescript/lib/tsserver.js", "");
    let command = runtime::detect_with_path(&project, &config, Some(&path))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.program, server);
    assert_eq!(command.version, "server-1");
    assert_eq!(command.args, ["literal; argument"]);
    assert_eq!(
        command.typescript_sdk,
        Some(fixture.0.join("node_modules/typescript/lib"))
    );
    assert!(
        command
            .runtime_versions
            .iter()
            .any(|version| version == "typescript: 5.9.3")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_probes_are_bounded_and_rustup_proxy_name_is_preserved() {
    let fixture = Fixture::new("code-probe-bounds");
    let binary = executable(&fixture, "bin/proxy", "#!/bin/sh\nwhile :; do :; done\n");
    std::os::unix::fs::symlink(&binary, fixture.0.join("bin/rust-analyzer")).unwrap();
    let path = fixture.0.join("bin").into_os_string();
    let resolved = runtime::find_program("rust-analyzer", &fixture.0, Some(&path)).unwrap();
    assert_eq!(resolved.file_name().unwrap(), "rust-analyzer");
    assert!(
        runtime::run(&resolved, &[], &fixture.0, Duration::from_millis(30))
            .await
            .unwrap_err()
            .contains("timed out")
    );
    fixture.write("bin/plain", "not executable");
    assert!(runtime::find_program("plain", &fixture.0, Some(&path)).is_none());
    assert!(runtime::find_program("plain", &fixture.0, Some(Path::new(".").as_os_str())).is_none());
    let large = executable(&fixture, "bin/large", "#!/bin/sh\nprintf '%40000s' ''\n");
    assert!(
        runtime::run(&large, &[], &fixture.0, Duration::from_secs(1))
            .await
            .unwrap_err()
            .contains("limit")
    );
    assert!(!fixture.0.join("Cargo.lock").exists());
}
