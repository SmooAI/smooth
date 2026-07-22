//! `th mcp serve` — run `th` itself as an MCP server over stdio.
//!
//! The inverse of the `th mcp add/list/...` client commands (which register
//! OTHER servers for the operator): this turns `th` into a server that Claude
//! Desktop / Cursor / Windsurf / VS Code consume, exposing th's high-value
//! surfaces as MCP tools. This is the load-bearing primitive for the "th as a
//! lead magnet into Claude Desktop" epic (th-63e572): the same tool layer is
//! reused by the stdio transport here and, later, by a hosted Streamable-HTTP
//! server at `mcp.smoo.ai`.
//!
//! **stdout is the JSON-RPC channel.** Nothing in this module (or the tools it
//! calls) may write to stdout — only `tracing`/stderr. `serve_stdio` takes over
//! stdin/stdout for the protocol.
//!
//! Spike scope (th-aa4c32): the local, no-login surfaces — pearls. Memory and
//! the gated org surfaces (`th api smooth-operator`, knowledge, LLM gateway)
//! layer on next, behind the Sign-in-with-Smoo conversion moment.

use anyhow::Result;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::Deserialize;

use smooth_pearls::{NewPearl, PearlType, Priority};

/// The Smooth MCP server. Stateless beyond the tool router — each tool opens
/// the pearl store fresh (matching `th pearls` CLI semantics), so the server
/// tracks the workspace it's launched in with no long-lived DB handle.
#[derive(Clone)]
pub struct SmoothMcp {
    tool_router: ToolRouter<Self>,
}

impl SmoothMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

/// Arguments for `pearls_create`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatePearlArgs {
    /// Short title for the work item.
    pub title: String,
    /// Why this exists and what needs doing. Optional.
    #[serde(default)]
    pub description: String,
    /// One of: task, bug, feature, epic, chore. Defaults to task.
    #[serde(default)]
    pub pearl_type: Option<String>,
    /// Priority 0–4 (0 = critical, 2 = medium, 4 = backlog). Defaults to 2.
    #[serde(default)]
    pub priority: Option<u8>,
}

#[tool_router(router = tool_router)]
impl SmoothMcp {
    /// Pearls that are ready to work on right now (open, no unresolved
    /// blockers), highest priority first. This is `th pearls ready`.
    ///
    /// # Errors
    /// MCP error if no pearl store exists in the workspace or the query fails.
    #[tool(
        name = "pearls_ready",
        description = "List work items (pearls) that are ready to work on now — open, unblocked, highest priority first."
    )]
    pub async fn pearls_ready(&self) -> Result<String, ErrorData> {
        let (store, _dir) = crate::open_pearl_store_with_path().map_err(|e| store_err(&e))?;
        let ready = store.ready().map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        if ready.is_empty() {
            return Ok("No pearls are ready to work on.".to_string());
        }
        let mut out = format!("{} pearl(s) ready:\n", ready.len());
        for p in &ready {
            let _ = writeln!(out, "- {} [P{}] {}", p.id, p.priority.as_u8(), p.title);
        }
        Ok(out)
    }

    /// Create a new pearl (work item).
    ///
    /// # Errors
    /// MCP error for an unknown pearl type / out-of-range priority, a missing
    /// pearl store, or a failed write.
    #[tool(name = "pearls_create", description = "Create a new work item (pearl). Returns the new pearl id.")]
    pub async fn pearls_create(&self, params: Parameters<CreatePearlArgs>) -> Result<String, ErrorData> {
        let args = params.0;
        let pearl_type = match args.pearl_type.as_deref() {
            None | Some("") => PearlType::Task,
            Some(s) => PearlType::from_str_loose(s)
                .ok_or_else(|| ErrorData::invalid_params(format!("unknown pearl type '{s}' (task|bug|feature|epic|chore)"), None))?,
        };
        let priority = match args.priority {
            None => Priority::Medium,
            Some(v) => Priority::from_u8(v).ok_or_else(|| ErrorData::invalid_params(format!("priority {v} out of range (0–4)"), None))?,
        };
        let (store, _dir) = crate::open_pearl_store_with_path().map_err(|e| store_err(&e))?;
        let pearl = store
            .create(&NewPearl {
                title: args.title,
                description: args.description,
                pearl_type,
                priority,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
            })
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(format!("Created pearl {} — {}", pearl.id, pearl.title))
    }
}

/// Map a "no pearl store here" open failure to an actionable MCP error.
fn store_err(e: &anyhow::Error) -> ErrorData {
    ErrorData::internal_error(
        format!("no pearl store in this workspace ({e}). Run `th pearls init` in a repo, or launch the server with its cwd set to a repo that has one."),
        None,
    )
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SmoothMcp {
    fn get_info(&self) -> ServerInfo {
        // `Implementation` is #[non_exhaustive]; start from the build-env values
        // (name/version from CARGO_PKG_*) and override the presentation fields.
        let mut server_info = Implementation::from_build_env();
        server_info.name = "smooth".to_string();
        server_info.title = Some("Smooth (th)".to_string());
        server_info.description =
            Some("Smoo AI's th CLI, exposed as MCP tools: pearls (work tracking) now; memory and the org agent behind Sign in with Smoo next.".to_string());
        server_info.website_url = Some("https://smoo.ai".to_string());
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
            .with_instructions(
                "Smooth exposes `th` work-tracking as MCP tools. Use `pearls_ready` to see what's \
                 ready to work on and `pearls_create` to file new work. These act on the pearl \
                 store in the workspace this server was launched in."
                    .to_string(),
            )
    }
}

/// Serve the Smooth MCP surface over stdio until the client disconnects.
///
/// Takes over stdin/stdout for JSON-RPC. Returns when the peer closes.
///
/// # Errors
/// Returns an error if the stdio transport fails to initialize or the service
/// loop terminates abnormally.
pub async fn serve_stdio() -> Result<()> {
    let service = SmoothMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full protocol round-trip over an in-memory duplex: a real MCP client
    /// initializes against SmoothMcp and lists its tools. Exercises the
    /// handshake, capability advertisement, server identity, and the tool
    /// router — everything except the pearl-store-backed tool bodies (which
    /// need a workspace and are covered by smooth-pearls' own tests).
    #[tokio::test]
    async fn initialize_and_list_tools_round_trip() {
        let (server_t, client_t) = tokio::io::duplex(8 * 1024);

        let server = tokio::spawn(async move {
            let svc = SmoothMcp::new().serve(server_t).await.expect("server init");
            svc.waiting().await.expect("server run");
        });

        let client = ().serve(client_t).await.expect("client init");

        // Server identity + instructions came back on initialize.
        let info = client.peer_info().expect("peer info present");
        assert_eq!(info.server_info.name, "smooth");
        assert!(info.instructions.as_ref().is_some_and(|s| s.contains("pearls")));

        // Tools are advertised.
        let tools = client.list_tools(None).await.expect("list tools");
        let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"pearls_ready"), "missing pearls_ready in {names:?}");
        assert!(names.contains(&"pearls_create"), "missing pearls_create in {names:?}");

        client.cancel().await.expect("client shutdown");
        server.abort();
    }
}
