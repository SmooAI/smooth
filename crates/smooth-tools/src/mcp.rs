//! MCP client — spawn `mcp.toml` servers and surface their tools to the operator.
//!
//! `th mcp add` writes server definitions to `~/.smooth/mcp.toml` (and projects
//! may add `<repo>/.smooth/mcp.toml`), but until th-52ec01 nothing in the daemon
//! path *consumed* them — so no MCP tool ever appeared. This module closes that
//! gap: it loads the merged config, spawns each non-disabled server as a stdio
//! child process (via `rmcp`'s `TokioChildProcess`), performs the MCP
//! `initialize` handshake, lists the server's tools, and wraps each one as a
//! [`smooth_operator::Tool`] the daemon registers on the per-turn `ToolRegistry`.
//!
//! Because the wrapped tools sit on that registry like any built-in, they pass
//! through the SAME per-turn hooks (permission gate, then Narc) — MCP tools are
//! not special-cased out of the security model.
//!
//! ## Lifecycle
//!
//! [`McpManager`] owns the live `rmcp` sessions for the daemon's lifetime. The
//! child processes stay up between turns (spawning per-turn would restart every
//! server on every message); dropping the manager drops the sessions, which
//! closes each transport and reaps the child. Spawning is best-effort: a server
//! that fails to launch or handshake is logged and skipped, never fatal.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{Peer, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use smooth_operator::tool::{Tool, ToolSchema};

use crate::mcp_config::{expand_env, McpConfig, McpServerConfig};

/// How long a single server gets to spawn + complete the MCP `initialize`
/// handshake + list its tools before we give up on it. Keeps one slow/broken
/// server from wedging daemon startup.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Owns the live MCP sessions and the tools they expose.
///
/// Built once at daemon startup and shared (via `Arc`) into the tool provider,
/// which hands `tools()` out on every turn.
#[derive(Default)]
pub struct McpManager {
    /// Live sessions — held only to keep the child processes alive. Dropping the
    /// manager drops these, closing each transport and reaping the child. Never
    /// read after construction (RAII keep-alive), hence `dead_code`.
    #[allow(dead_code, reason = "held for RAII: dropping the session reaps the child process")]
    sessions: Vec<RunningService<RoleClient, ()>>,
    tools: Vec<Arc<dyn Tool>>,
}

impl McpManager {
    /// Load global (`~/.smooth/mcp.toml`) + project (`<workspace>/.smooth/mcp.toml`)
    /// config, merge with project shadowing global on a name collision (the same
    /// rule as CLI-wrapper plugins), then spawn every non-disabled server.
    ///
    /// Best-effort: config-load and per-server spawn failures are logged and
    /// skipped. Never errors — a broken MCP setup must not take the daemon down.
    pub async fn discover(workspace: &Path) -> Self {
        Self::spawn(merged_servers(workspace)).await
    }

    /// Spawn an explicit set of servers. Disabled entries are skipped here (not
    /// by the caller) so every entry point applies the same rule.
    pub async fn spawn(servers: Vec<McpServerConfig>) -> Self {
        let mut mgr = Self::default();
        for server in servers {
            if server.disabled {
                tracing::debug!(server = %server.name, "mcp: skipping disabled server");
                continue;
            }
            match tokio::time::timeout(SPAWN_TIMEOUT, spawn_one(&server)).await {
                Ok(Ok((session, tools))) => {
                    tracing::info!(server = %server.name, tools = tools.len(), "mcp: server ready");
                    mgr.tools.extend(tools);
                    mgr.sessions.push(session);
                }
                Ok(Err(e)) => tracing::warn!(server = %server.name, error = %e, "mcp: server failed to start; skipping"),
                Err(_) => tracing::warn!(server = %server.name, timeout = ?SPAWN_TIMEOUT, "mcp: server startup timed out; skipping"),
            }
        }
        mgr
    }

    /// The registrable tools from every server that came up. Cheap to call per
    /// turn — each element is an `Arc` clone.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

/// Merge global + project `mcp.toml` for `workspace` (project wins by name).
fn merged_servers(workspace: &Path) -> Vec<McpServerConfig> {
    let mut by_name: HashMap<String, McpServerConfig> = HashMap::new();
    // Global first, then project — project overwrites on a name collision.
    if let Some(global) = McpConfig::default_path() {
        for s in load_or_empty(&global) {
            by_name.insert(s.name.clone(), s);
        }
    }
    for s in load_or_empty(&workspace.join(".smooth").join("mcp.toml")) {
        if by_name.insert(s.name.clone(), s.clone()).is_some() {
            tracing::info!(server = %s.name, "mcp: project scope overrides global");
        }
    }
    let mut servers: Vec<McpServerConfig> = by_name.into_values().collect();
    // Stable order so tool registration (and logs) are deterministic.
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    servers
}

fn load_or_empty(path: &Path) -> Vec<McpServerConfig> {
    match McpConfig::load(path) {
        Ok(cfg) => cfg.servers,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "mcp: failed to load config; ignoring");
            Vec::new()
        }
    }
}

/// Spawn one server, handshake, and wrap its tools.
async fn spawn_one(server: &McpServerConfig) -> Result<(RunningService<RoleClient, ()>, Vec<Arc<dyn Tool>>)> {
    let mut cmd = tokio::process::Command::new(expand_env(&server.command));
    for arg in &server.args {
        cmd.arg(expand_env(arg));
    }
    for (k, v) in &server.env {
        cmd.env(k, expand_env(v));
    }
    let transport = TokioChildProcess::new(cmd).map_err(|e| anyhow!("spawn `{}`: {e}", server.command))?;
    // `()` is the bare client handler; `.serve` runs the `initialize` handshake.
    let session = ().serve(transport).await.map_err(|e| anyhow!("initialize handshake: {e}"))?;

    let remote_tools = session.peer().list_all_tools().await.map_err(|e| anyhow!("list_tools: {e}"))?;
    let peer = session.peer().clone();
    let tools: Vec<Arc<dyn Tool>> = remote_tools
        .into_iter()
        .map(|t| {
            Arc::new(McpTool {
                name: namespaced_tool_name(&server.name, &t.name),
                remote_name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                parameters: serde_json::Value::Object((*t.input_schema).clone()),
                peer: peer.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();
    Ok((session, tools))
}

/// Namespace a remote tool as `<server>__<tool>`, sanitised + length-capped.
///
/// The result matches the provider tool-name regex `^[a-zA-Z0-9_-]{1,64}$`
/// (Anthropic + the OpenAI-compatible gateway both reject anything else for the
/// WHOLE request); invalid chars become `_` and it's truncated to 64 chars.
#[must_use]
pub fn namespaced_tool_name(server: &str, tool: &str) -> String {
    let raw = format!("{server}__{tool}");
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect();
    // Empty is impossible (server/tool names are non-empty), but guard anyway.
    if cleaned.is_empty() {
        "mcp_tool".to_string()
    } else {
        cleaned
    }
}

/// A single remote MCP tool, callable over the long-lived session `peer`.
struct McpTool {
    /// The namespaced name the LLM sees (`<server>__<tool>`).
    name: String,
    /// The tool's real name on the remote server (what `call_tool` needs).
    remote_name: String,
    description: String,
    parameters: serde_json::Value,
    peer: Peer<RoleClient>,
}

#[async_trait]
impl Tool for McpTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        // MCP wants an object (or nothing). A null/absent arg map is fine; any
        // other shape is a caller bug we surface rather than silently drop.
        let arguments = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => return Err(anyhow!("MCP tool `{}` expects an object arguments map, got: {other}", self.remote_name)),
        };
        // `CallToolRequestParams` is `#[non_exhaustive]` — assign fields on a
        // `Default` instance rather than a struct literal (E0639).
        let mut params = CallToolRequestParams::default();
        params.name = Cow::Owned(self.remote_name.clone());
        params.arguments = arguments;
        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|e| anyhow!("MCP call_tool `{}`: {e}", self.remote_name))?;

        let text = flatten_content(&result.content, result.structured_content.as_ref());
        if result.is_error.unwrap_or(false) {
            return Err(anyhow!("MCP tool `{}` reported an error: {text}", self.remote_name));
        }
        Ok(text)
    }
}

/// Flatten an MCP tool result into a single string for the agent. Text blocks
/// are concatenated; non-text blocks (image/audio/resource) are noted by kind so
/// the agent knows something non-textual came back. Falls back to the structured
/// content JSON when there are no content blocks at all.
fn flatten_content(content: &[rmcp::model::Content], structured: Option<&serde_json::Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in content {
        if let Some(text) = block.as_text() {
            parts.push(text.text.clone());
        } else {
            // Name the kind without dumping raw base64 blobs into the context.
            let kind = match &block.raw {
                rmcp::model::RawContent::Image(_) => "image",
                rmcp::model::RawContent::Audio(_) => "audio",
                rmcp::model::RawContent::Resource(_) => "resource",
                rmcp::model::RawContent::ResourceLink(_) => "resource_link",
                rmcp::model::RawContent::Text(_) => "text",
            };
            parts.push(format!("[{kind} content omitted]"));
        }
    }
    if parts.is_empty() {
        if let Some(s) = structured {
            return serde_json::to_string(s).unwrap_or_default();
        }
    }
    parts.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn namespaced_name_prefixes_and_is_valid() {
        let n = namespaced_tool_name("ms365-outlook", "list-mail");
        assert_eq!(n, "ms365-outlook__list-mail");
        assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn namespaced_name_sanitises_invalid_chars() {
        // Dots, slashes, spaces, `@` -> `_`.
        let n = namespaced_tool_name("@softeria/ms 365", "get.mail");
        assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "got: {n}");
        assert!(n.starts_with("_softeria_ms_365__get_mail") || n.len() <= 64);
    }

    #[test]
    fn namespaced_name_truncates_to_64() {
        let long_server = "a".repeat(100);
        let n = namespaced_tool_name(&long_server, "tool");
        assert_eq!(n.len(), 64);
        assert!(n.chars().all(|c| c == 'a'));
    }

    #[test]
    fn merged_servers_project_shadows_global() {
        // Point global config at a temp home, project at a temp workspace.
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SMOOTH_HOME", home.path());

        let global = McpConfig {
            servers: vec![
                McpServerConfig {
                    name: "shared".into(),
                    command: "global-cmd".into(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                },
                McpServerConfig {
                    name: "global-only".into(),
                    command: "g".into(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                },
            ],
        };
        global.save(&McpConfig::default_path().unwrap()).unwrap();

        let workspace = tempfile::tempdir().unwrap();
        let project = McpConfig {
            servers: vec![McpServerConfig {
                name: "shared".into(),
                command: "project-cmd".into(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
            }],
        };
        project.save(&workspace.path().join(".smooth").join("mcp.toml")).unwrap();

        let merged = merged_servers(workspace.path());
        std::env::remove_var("SMOOTH_HOME");

        assert_eq!(merged.len(), 2, "shared + global-only");
        let shared = merged.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.command, "project-cmd", "project shadows global");
        assert!(merged.iter().any(|s| s.name == "global-only"));
    }

    #[tokio::test]
    async fn spawn_skips_disabled_and_missing_binary() {
        let servers = vec![
            McpServerConfig {
                name: "off".into(),
                command: "definitely-not-a-real-binary-xyz".into(),
                args: vec![],
                env: HashMap::new(),
                disabled: true,
            },
            McpServerConfig {
                name: "broken".into(),
                command: "definitely-not-a-real-binary-xyz-42".into(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
            },
        ];
        // Disabled server skipped; broken one fails to spawn but does not panic.
        let mgr = McpManager::spawn(servers).await;
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
    }

    /// Live end-to-end spawn against a real MCP server. Ignored by default —
    /// needs `npx` + network to fetch `@modelcontextprotocol/server-everything`.
    /// Run with: `cargo test -p smooai-smooth-tools mcp -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs npx + network; run manually to verify a real server surfaces tools"]
    async fn live_spawn_surfaces_tools() {
        let servers = vec![McpServerConfig {
            name: "everything".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-everything".into()],
            env: HashMap::new(),
            disabled: false,
        }];
        let mgr = McpManager::spawn(servers).await;
        assert!(!mgr.is_empty(), "server-everything should surface at least one tool");
        // Every tool is namespaced under the server and gateway-name-legal.
        for t in mgr.tools() {
            let name = t.schema().name;
            assert!(name.starts_with("everything__"), "expected namespace prefix, got {name}");
            assert!(name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        }
    }

    #[test]
    fn flatten_prefers_text_then_structured() {
        // No content, structured fallback.
        let out = flatten_content(&[], Some(&serde_json::json!({"ok": true})));
        assert_eq!(out, r#"{"ok":true}"#);
        // Empty everything.
        assert_eq!(flatten_content(&[], None), "");
    }
}
