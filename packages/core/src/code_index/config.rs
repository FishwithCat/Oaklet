use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Language;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeIndexConfig {
    #[serde(default)]
    pub servers: BTreeMap<Language, ServerConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

fn enabled() -> bool {
    true
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: None,
            args: Vec::new(),
        }
    }
}

impl CodeIndexConfig {
    pub fn validate(&mut self) -> Result<(), String> {
        for server in self.servers.values_mut() {
            if let Some(command) = &mut server.command {
                *command = command.trim().to_string();
                if command.is_empty() || command.contains('\0') {
                    return Err(
                        "Code index server command must be nonempty and contain no NUL.".into(),
                    );
                }
            }
            if server.args.iter().any(|arg| arg.contains('\0')) {
                return Err("Code index server arguments must contain no NUL.".into());
            }
            if server.command.is_none() && !server.args.is_empty() {
                return Err(
                    "Set a code index server command when overriding its arguments.".into(),
                );
            }
        }
        Ok(())
    }
}
