//! MCP (Model Context Protocol) client — spawn user-configured MCP
//! servers as stdio subprocesses and bridge their tools into the
//! runner's `ToolRegistry`.
//!
//! ## Config format
//!
//! `~/.smooth/mcp.toml` (resolved from `$SMOOTH_HOME/mcp.toml` inside
//! the sandbox if set, else `~/.smooth/mcp.toml`). Users add servers
//! with `th mcp add <name> <command> [args...]`. Example:
//!
//! ```toml
//! [[servers]]
//! name = "playwright"
//! command = "npx"
//! args = ["@playwright/mcp@latest"]
//! env = { BROWSER = "chromium" }
//!
//! [[servers]]
//! name = "github"
//! command = "docker"
//! args = ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"]
//! env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${env:GITHUB_TOKEN}" }
//! ```
//!
//! Per-server `env` entries support `${env:VAR}` substitution so the
//! config doesn't have to store raw secrets — the runner pulls them
//! from its own environment at load time.
//!
//! ## How it integrates
//!
//! At runner startup, [`load_and_register_mcp_servers`] reads the
//! config, spawns every server, handshakes, calls `tools/list`, wraps
//! each tool as a [`McpTool`] (which implements `smooth_operator::Tool`),
//! and returns them. The caller registers them with its `ToolRegistry`.
//!
//! Servers are named so their tools are prefixed:
//! `playwright.browser_navigate`, `github.create_issue`, etc. This keeps
//! them from colliding with the runner's native tools and makes it
//! obvious which server a tool came from.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, Tool as McpToolDef};
use rmcp::service::{RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use rmcp::RoleClient;
use serde::{Deserialize, Serialize};
use smooth_operator::tool::{Tool, ToolSchema};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    /// Name used to prefix this server's tools. Must be unique across
    /// the config file.
    pub name: String,
    /// The executable to spawn (e.g. `npx`, `docker`, an absolute path).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra env vars for the spawned process. Values may reference the
    /// runner's environment via `${env:VAR_NAME}` — useful for passing
    /// secrets without hardcoding them in the config file.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Optional: skip this server without deleting its config entry.
    #[serde(default)]
    pub disabled: bool,
}

impl McpConfig {
    /// Resolve the default config path.
    ///
    /// 1. `$SMOOTH_HOME/mcp.toml` if set
    /// 2. `~/.smooth/mcp.toml` otherwise
    ///
    /// Returns `None` if neither can be resolved.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("SMOOTH_HOME") {
            return Some(PathBuf::from(home).join("mcp.toml"));
        }
        dirs_next::home_dir().map(|h| h.join(".smooth").join("mcp.toml"))
    }

    /// Load config from `path`. Returns an empty config if the file
    /// doesn't exist (not an error).
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    /// Persist to `path`, creating parent dirs as needed.
    #[allow(dead_code)] // Used by `th mcp add` (separate crate, lands next).
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }
}

/// Expand `${env:VAR}` references inside a string using the runner's
/// current environment. Unset variables expand to empty strings.
fn expand_env(input: &str) -> String {
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

// ---------------------------------------------------------------------------
// McpTool — smooth_operator::Tool implementation backed by an rmcp client
// ---------------------------------------------------------------------------

pub struct McpTool {
    /// `<server_name>.<remote_tool_name>` — e.g. `playwright.browser_navigate`.
    tool_name: String,
    /// The original tool name on the MCP server (no server prefix). Sent
    /// when we actually call the tool.
    remote_name: String,
    description: String,
    parameters: serde_json::Value,
    service: Arc<RunningService<RoleClient, ()>>,
}

#[async_trait]
impl Tool for McpTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        // rmcp expects `Option<JsonObject>` for params — normalize to an
        // empty object when the agent passes a non-object.
        let arguments = match args {
            serde_json::Value::Object(m) => Some(m),
            serde_json::Value::Null => None,
            other => Some(std::iter::once(("input".into(), other)).collect::<serde_json::Map<_, _>>()),
        };

        let mut params = CallToolRequestParams::new(self.remote_name.clone());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }

        let result = self
            .service
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP tool `{}` failed: {e}", self.tool_name))?;

        // Serialize the result content blocks into a single string. MCP
        // tools return Vec<Content> where each content is text/image/etc.
        // For the agent, we flatten text blocks and summarize the rest.
        let mut out = String::new();
        for block in &result.content {
            // Serialize each content block as JSON — rmcp's Content is
            // enum-ish; this preserves its shape for the LLM.
            match serde_json::to_string(&block) {
                Ok(s) => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&s);
                }
                Err(_) => out.push_str("(unserializable content block)"),
            }
        }
        if result.is_error.unwrap_or(false) {
            return Err(anyhow::anyhow!("MCP tool returned error: {out}"));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Server lifecycle: spawn + initialize + list_tools + wrap
// ---------------------------------------------------------------------------

/// Spawn one MCP server and return every tool it exposes (wrapped).
/// Server-level errors (spawn failure, handshake failure) become an
/// error return so the caller can log + skip; a single broken server
/// shouldn't kill the whole runner.
pub async fn connect_server(cfg: &McpServerConfig) -> anyhow::Result<Vec<Arc<McpTool>>> {
    use tokio::process::Command;

    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args);
    for (k, v) in &cfg.env {
        cmd.env(k, expand_env(v));
    }

    let transport = TokioChildProcess::new(cmd).map_err(|e| anyhow::anyhow!("spawn MCP server `{}`: {e}", cfg.name))?;

    let service = ().serve(transport).await.map_err(|e| anyhow::anyhow!("MCP handshake with `{}` failed: {e}", cfg.name))?;
    let service = Arc::new(service);

    // Fetch the tool list. rmcp gives us a `tools/list` helper that
    // paginates automatically.
    let tools = service
        .list_all_tools()
        .await
        .map_err(|e| anyhow::anyhow!("list_tools on `{}` failed: {e}", cfg.name))?;

    let mut wrapped = Vec::with_capacity(tools.len());
    for tool in tools {
        wrapped.push(Arc::new(wrap_mcp_tool(&cfg.name, &tool, Arc::clone(&service))));
    }
    tracing::info!(server = %cfg.name, tool_count = wrapped.len(), "MCP: connected and loaded tools");
    Ok(wrapped)
}

fn wrap_mcp_tool(server_name: &str, def: &McpToolDef, service: Arc<RunningService<RoleClient, ()>>) -> McpTool {
    let remote_name = def.name.to_string();
    let prefixed = format!("{server_name}.{remote_name}");
    let description = def
        .description
        .clone()
        .map_or_else(|| format!("MCP tool `{remote_name}` from `{server_name}`"), |s| s.to_string());
    // rmcp's Tool.input_schema is `Arc<JsonObject>`. Flatten to a Value
    // for smooth_operator's schema.
    let parameters = serde_json::Value::Object((*def.input_schema).clone());
    McpTool {
        tool_name: prefixed,
        remote_name,
        description,
        parameters,
        service,
    }
}

/// Load the config from `path` and connect every enabled server.
/// Returns every wrapped tool across all servers plus a list of
/// `(server_name, error)` for servers that failed to start — the
/// caller can log those without aborting the whole runner.
pub async fn load_and_register_mcp_servers(config_path: &std::path::Path) -> (Vec<Arc<dyn Tool>>, Vec<(String, String)>) {
    let config = match McpConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, path = %config_path.display(), "MCP: failed to load config; no MCP tools will be registered");
            return (Vec::new(), vec![("<config>".into(), e.to_string())]);
        }
    };
    if config.servers.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut failures = Vec::new();
    for server in &config.servers {
        if server.disabled {
            continue;
        }
        match connect_server(server).await {
            Ok(tools) => {
                for t in tools {
                    all_tools.push(t);
                }
            }
            Err(e) => {
                tracing::warn!(server = %server.name, error = %e, "MCP: server failed to start");
                failures.push((server.name.clone(), e.to_string()));
            }
        }
    }
    (all_tools, failures)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_substitutes_present_vars() {
        std::env::set_var("SMOOTH_TEST_EXPAND", "hello");
        assert_eq!(expand_env("prefix-${env:SMOOTH_TEST_EXPAND}-suffix"), "prefix-hello-suffix");
        std::env::remove_var("SMOOTH_TEST_EXPAND");
    }

    #[test]
    fn expand_env_handles_missing_vars_as_empty() {
        std::env::remove_var("SMOOTH_TEST_MISSING_XYZ");
        assert_eq!(expand_env("a-${env:SMOOTH_TEST_MISSING_XYZ}-b"), "a--b");
    }

    #[test]
    fn expand_env_handles_unterminated() {
        assert_eq!(expand_env("prefix-${env:UNCLOSED"), "prefix-${env:UNCLOSED");
    }

    #[test]
    fn expand_env_passes_through_literal_strings() {
        assert_eq!(expand_env("no substitutions here"), "no substitutions here");
    }

    #[test]
    fn config_load_missing_returns_empty() {
        let cfg = McpConfig::load(std::path::Path::new("/nonexistent/xyz/mcp.toml")).expect("missing = empty");
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn config_roundtrip() {
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
        cfg.save(&path).expect("save");

        let loaded = McpConfig::load(&path).expect("load");
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.servers[0].name, "playwright");
        assert_eq!(loaded.servers[0].command, "npx");
        assert_eq!(loaded.servers[0].env.get("BROWSER"), Some(&"chromium".to_string()));
    }
}
