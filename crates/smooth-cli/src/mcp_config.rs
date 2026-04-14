//! MCP server config — TOML schema shared with `smooth-operator-runner`.
//!
//! The runner is the consumer; this module exists so `th mcp` commands can
//! manage `~/.smooth/mcp.toml` without pulling rmcp into the CLI binary.
//! Keep the schema in lockstep with `crates/smooth-operator-runner/src/mcp.rs`
//! (`McpConfig` / `McpServerConfig`) — they round-trip through the same file.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

impl McpConfig {
    /// Default config path: `$SMOOTH_HOME/mcp.toml` if set, else
    /// `~/.smooth/mcp.toml`.
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("SMOOTH_HOME") {
            return Some(PathBuf::from(home).join("mcp.toml"));
        }
        dirs_next::home_dir().map(|h| h.join(".smooth").join("mcp.toml"))
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let len = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() < len
    }
}

/// Expand `${env:VAR}` references using the current process environment.
/// Unset variables expand to empty strings. Unterminated references are
/// passed through verbatim.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("${env:") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 6..];
        if let Some(end) = after.find('}') {
            let var = &after[..end];
            out.push_str(&std::env::var(var).unwrap_or_default());
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[idx..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");

        let mut cfg = McpConfig::default();
        cfg.servers.push(McpServerConfig {
            name: "playwright".into(),
            command: "npx".into(),
            args: vec!["@playwright/mcp@latest".into()],
            env: HashMap::from([("BROWSER".into(), "chromium".into())]),
            disabled: false,
        });
        cfg.save(&path).unwrap();

        let loaded = McpConfig::load(&path).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.find("playwright").unwrap().command, "npx");

        let mut loaded = loaded;
        assert!(loaded.remove("playwright"));
        assert!(loaded.servers.is_empty());
        assert!(!loaded.remove("playwright"));
    }

    #[test]
    fn expand_env_substitutes_and_handles_missing() {
        std::env::set_var("SMOOTH_CLI_TEST_VAR", "value");
        assert_eq!(expand_env("${env:SMOOTH_CLI_TEST_VAR}"), "value");
        std::env::remove_var("SMOOTH_CLI_TEST_VAR");

        std::env::remove_var("SMOOTH_CLI_TEST_MISSING");
        assert_eq!(expand_env("a-${env:SMOOTH_CLI_TEST_MISSING}-b"), "a--b");
        assert_eq!(expand_env("prefix-${env:UNCLOSED"), "prefix-${env:UNCLOSED");
    }
}
