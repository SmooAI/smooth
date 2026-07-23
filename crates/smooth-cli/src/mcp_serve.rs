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
//! Two tiers (th-aa4c32 spiked the local tier; th-03943a added the org tier):
//! - **Local, free, no sign-in**: `pearls_ready` / `pearls_create` (the
//!   workspace pearl store) and `remember` / `recall` (local memory).
//! - **Your business, behind Sign in with Smoo (`th auth login`)**:
//!   `ask_business` — one turn of the Smooth Operator org agent over its SEP
//!   WebSocket transport (see `smooai::smooth_operator_ws`), which never sends
//!   or takes a destructive action unless the caller passes `approve: true`;
//!   and `knowledge_search` — a fast read of the org knowledge base.

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
use serde_json::json;

use smooth_pearls::{MemoryStore, NewPearl, PearlType, Priority};

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

/// Arguments for `remember`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberArgs {
    /// The fact or note to remember.
    pub content: String,
    /// Optional tag/source to group this memory under (defaults to "mcp").
    #[serde(default)]
    pub source: Option<String>,
}

/// Arguments for `recall`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallArgs {
    /// How many recent memories to return (default 10).
    #[serde(default)]
    pub limit: Option<u64>,
}

/// Arguments for `knowledge_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KnowledgeSearchArgs {
    /// What to search your org's knowledge base for.
    pub query: String,
    /// Max passages to return (default 5).
    #[serde(default)]
    pub max_results: Option<u64>,
}

/// Arguments for `ask_business` — one turn of the Smooth Operator org agent.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskBusinessArgs {
    /// What to ask or tell your business, in plain language.
    pub message: Option<String>,
    /// Continue an existing conversation (pass back the id a previous call returned).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Allow the operator to take destructive actions this turn — send email,
    /// write to the CRM/knowledge base. Default false: it declines them and
    /// tells you what it would have done, so you can re-run with approve=true.
    #[serde(default)]
    pub approve: Option<bool>,
    /// Act on a specific org id. Defaults to your active org.
    #[serde(default)]
    pub org: Option<String>,
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

    // ── Local memory (free, no sign-in) ─────────────────────────────────────

    /// Store a note in local memory (this workspace's store).
    ///
    /// # Errors
    /// MCP error if the local store can't be opened or the write fails.
    #[tool(name = "remember", description = "Save a note to local memory (kept in this workspace). Free — no sign-in.")]
    pub async fn remember(&self, params: Parameters<RememberArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let source = a.source.unwrap_or_else(|| "mcp".to_string());
        let id = open_memory_store()?
            .append(a.content, source)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(format!("Remembered ({id})."))
    }

    /// Recent notes from local memory.
    ///
    /// # Errors
    /// MCP error if the local store can't be opened or the query fails.
    #[tool(
        name = "recall",
        description = "List recent notes from local memory. Free — no sign-in.",
        annotations(read_only_hint = true)
    )]
    pub async fn recall(&self, params: Parameters<RecallArgs>) -> Result<String, ErrorData> {
        let limit = usize::try_from(params.0.limit.unwrap_or(10)).unwrap_or(10);
        let items = open_memory_store()?
            .list_recent(limit)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        if items.is_empty() {
            return Ok("No memories yet.".to_string());
        }
        let mut out = String::new();
        for m in &items {
            let _ = writeln!(out, "- {}", m.content);
        }
        Ok(out.trim_end().to_string())
    }

    // ── Your business (requires Sign in with Smoo: `th auth login`) ──────────

    /// Fast semantic read of your org's knowledge base.
    ///
    /// # Errors
    /// MCP error if not signed in to Smoo, no active org, or the request fails.
    #[tool(
        name = "knowledge_search",
        description = "Search your Smoo org's knowledge base and return the most relevant passages. Requires a signed-in Smoo session (`th auth login`, or an M2M key via `th api login`).",
        annotations(read_only_hint = true)
    )]
    pub async fn knowledge_search(&self, params: Parameters<KnowledgeSearchArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let client = crate::smooai::require_authed().await.map_err(|e| sign_in_err(&e))?;
        let org = active_org()?;
        let max = a.max_results.unwrap_or(5);
        let resp = client
            .post(
                &format!("/organizations/{org}/knowledge/search"),
                Some(&json!({ "query": a.query, "maxResults": max })),
            )
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let results = resp.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if results.is_empty() {
            return Ok("No matching passages in your knowledge base.".to_string());
        }
        let mut out = String::new();
        for (i, r) in results.iter().enumerate() {
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("(untitled)");
            let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let _ = writeln!(out, "{}. {name}\n   {content}\n", i + 1);
        }
        Ok(out.trim_end().to_string())
    }

    /// Talk to your business — one turn of the Smooth Operator org agent.
    ///
    /// The agent runs on your live Smoo org (CRM, inbox, knowledge, analytics)
    /// and answers in plain language, over its SEP WebSocket transport. It never
    /// sends email or takes a destructive action unless you pass `approve: true`
    /// — otherwise it declines the action, tells you what it would have done, and
    /// returns a `conversation_id` so you can re-run with `approve: true` to
    /// allow it (and continue the same thread).
    ///
    /// # Errors
    /// MCP error if not signed in to Smoo (user session), no active org, no
    /// `message`, or the operator call fails.
    #[tool(
        name = "ask_business",
        description = "Talk to your business in plain language. Smooth Operator (the agent on your Smoo org) answers questions about revenue/CRM/knowledge and can draft — and, only when you pass approve=true, send — email or other writes. Needs Sign in with Smoo (`th auth login`). Continue a thread by passing back the conversation_id it returns."
    )]
    pub async fn ask_business(&self, params: Parameters<AskBusinessArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        let message = a
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| ErrorData::invalid_params("Provide `message` to ask your business.".to_string(), None))?;
        let org = crate::active_org::resolve(a.org).map_err(|e| ErrorData::invalid_request(format!("No active Smoo org. {e}"), None))?;

        // The user session is minted into the SEP token inside `operator_turn`;
        // it errors (with a `th auth login` hint) when there's no Smoo session.
        // `approve` gates destructive tools inline: false (default) declines +
        // surfaces; true allows them this turn.
        let turn = crate::smooai::smooth_operator_ws::operator_turn(&org, message, a.conversation_id.as_deref(), a.approve.unwrap_or(false))
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;

        Ok(crate::smooai::smooth_operator_ws::render_operator_turn(&turn))
    }
}

/// Map a "no pearl store here" open failure to an actionable MCP error.
fn store_err(e: &anyhow::Error) -> ErrorData {
    ErrorData::internal_error(
        format!("no pearl store in this workspace ({e}). Run `th pearls init` in a repo, or launch the server with its cwd set to a repo that has one."),
        None,
    )
}

/// Open the local memory store for the workspace this server runs in.
fn open_memory_store() -> Result<MemoryStore, ErrorData> {
    let (store, _dir) = crate::open_pearl_store_with_path().map_err(|e| store_err(&e))?;
    Ok(MemoryStore::new(store.dolt().clone()))
}

/// The active Smoo org, or an actionable MCP error.
fn active_org() -> Result<String, ErrorData> {
    crate::active_org::resolve(None).map_err(|e| ErrorData::invalid_request(format!("No active Smoo org. {e}"), None))
}

/// Turn an auth failure into the Sign-in-with-Smoo prompt. The underlying error
/// already names the fix; we lead with the exact command so the model can relay
/// it verbatim to the user.
fn sign_in_err(e: &anyhow::Error) -> ErrorData {
    ErrorData::invalid_request(
        format!("Sign in to Smoo to use your business tools — run `th auth login` (Sign in with Smoo). ({e})"),
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
            Some("Talk to your business. Smoo AI's `th` exposed as MCP tools: local work + memory for free, and the Smooth Operator org agent behind Sign in with Smoo.".to_string());
        server_info.website_url = Some("https://smoo.ai".to_string());
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
            .with_instructions(
                "Smooth exposes your Smoo AI `th` CLI as MCP tools, in two tiers.\n\n\
                 LOCAL — free, no sign-in. `pearls_ready` / `pearls_create` track work in the pearl \
                 store of the workspace this server was launched in. `remember` / `recall` keep local notes.\n\n\
                 YOUR BUSINESS — requires Sign in with Smoo (tell the user to run `th auth login`). \
                 `ask_business` talks to Smooth Operator, the agent that runs on the user's live Smoo org: \
                 ask about revenue and the pipeline, search the CRM, and draft — or, with the user's explicit \
                 approval, send — email. It acts on the active org and NEVER sends or takes a destructive action \
                 without approval; when it pauses on one, relay the pending action and approve only if the user \
                 says so (call `ask_business` again with approve=true and the returned conversation_id). \
                 `knowledge_search` is a fast read of the org knowledge base.\n\n\
                 When an org tool reports the user isn't signed in, tell them to run `th auth login` — don't retry blindly."
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

        // Tools are advertised — local (free) and org (gated) alike.
        let tools = client.list_tools(None).await.expect("list tools");
        let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
        for expected in ["pearls_ready", "pearls_create", "remember", "recall", "knowledge_search", "ask_business"] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }

        // Read-only tools carry the annotation; the org agent does not.
        let recall = tools.tools.iter().find(|t| t.name == "recall").expect("recall present");
        assert_eq!(
            recall.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(true),
            "recall should be read-only"
        );
        let ask = tools.tools.iter().find(|t| t.name == "ask_business").expect("ask_business present");
        assert_ne!(
            ask.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(true),
            "ask_business is not read-only"
        );

        // Instructions surface the sign-in path for org tools.
        let info = client.peer_info().expect("peer info present");
        assert!(
            info.instructions.as_ref().is_some_and(|s| s.contains("th auth login")),
            "instructions should name the sign-in command"
        );

        client.cancel().await.expect("client shutdown");
        server.abort();
    }
}
