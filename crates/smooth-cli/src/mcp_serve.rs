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
//!   `ask_business` — one turn of the Smooth Operator org agent (the same
//!   user-only `POST /organizations/{org}/smooth-operator/chat` the
//!   `th api smooth-operator` CLI drives), which never sends or takes a
//!   destructive action without explicit approval; and `knowledge_search` —
//!   a fast read of the org knowledge base.

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

use crate::smooai::user_client::UserClient;

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
    /// What to ask or tell your business, in plain language. Omit only when
    /// approving a pending action (set `approve` + `conversation_id` instead).
    #[serde(default)]
    pub message: Option<String>,
    /// Continue an existing conversation (returned by a previous call).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Approve (true) or decline (false) an action the operator paused on.
    /// Requires `conversation_id`. The operator never sends/acts without this.
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
    /// and answers in plain language. It never sends email or takes a
    /// destructive action without explicit approval: when it pauses on one,
    /// this returns the pending action and a `conversation_id`; approve it by
    /// calling again with `approve: true` and that `conversation_id`.
    ///
    /// # Errors
    /// MCP error if not signed in to Smoo (user session), no active org, no
    /// `message`/approval given, or the request fails.
    #[tool(
        name = "ask_business",
        description = "Talk to your business in plain language. Smooth Operator (the agent on your Smoo org) answers questions about revenue/CRM/knowledge and can draft — and, with your approval, send — email. Needs Sign in with Smoo (`th auth login`). To approve a paused action, call again with approve=true and the conversation_id it returned."
    )]
    pub async fn ask_business(&self, params: Parameters<AskBusinessArgs>) -> Result<String, ErrorData> {
        let a = params.0;
        // `from_user_session` IS the gate: the org agent is a user-only route and
        // 401s under M2M, so this errors (with a login hint) when there's no
        // Smoo user session.
        let client = UserClient::from_user_session().await.map_err(|e| sign_in_err(&e))?;
        let org = crate::active_org::resolve(a.org).map_err(|e| ErrorData::invalid_request(format!("No active Smoo org. {e}"), None))?;

        let turn = if let (Some(approve), Some(cid)) = (a.approve, a.conversation_id.as_deref()) {
            // Resolve a paused action the operator asked us to confirm.
            client
                .post(
                    &format!("/organizations/{org}/smooth-operator/confirm"),
                    &json!({ "conversationId": cid, "approve": approve }),
                )
                .await
        } else {
            let message = a.message.as_deref().filter(|m| !m.trim().is_empty()).ok_or_else(|| {
                ErrorData::invalid_params(
                    "Provide `message` to ask, or `approve` + `conversation_id` to resolve a pending action.".to_string(),
                    None,
                )
            })?;
            let mut body = json!({ "message": message });
            if let Some(cid) = &a.conversation_id {
                body["conversationId"] = json!(cid);
            }
            client.post(&format!("/organizations/{org}/smooth-operator/chat"), &body).await
        }
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        // Defense-in-depth on the approval path: a paused action with no
        // conversation id can't be approved, so fail loudly rather than emit a
        // send-prompt the user can never act on. (The API always returns one;
        // this only fires on a contract violation.)
        let paused = turn.get("pendingAction").is_some_and(|v| !v.is_null());
        let has_cid = turn.get("conversationId").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
        if paused && !has_cid {
            return Err(ErrorData::internal_error(
                "The operator paused on an action but returned no conversation id, so it can't be approved. Try again.".to_string(),
                None,
            ));
        }

        Ok(render_turn(&turn))
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

/// Render one Smooth Operator turn for the MCP client: the reply, any tools it
/// used, an approval prompt for a paused action, and the conversation id so the
/// caller can continue or approve.
fn render_turn(turn: &serde_json::Value) -> String {
    let reply = turn.get("reply").and_then(|v| v.as_str()).unwrap_or_default();
    let cid = turn.get("conversationId").and_then(|v| v.as_str()).unwrap_or_default();

    let mut out = String::new();
    if !reply.is_empty() {
        out.push_str(reply);
        out.push('\n');
    }
    if let Some(tools) = turn.get("toolCalls").and_then(|v| v.as_array()).filter(|t| !t.is_empty()) {
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get("name").and_then(|v| v.as_str())).collect();
        if !names.is_empty() {
            let _ = writeln!(out, "\n_(used: {})_", names.join(", "));
        }
    }
    if let Some(pa) = turn.get("pendingAction").filter(|v| !v.is_null()) {
        let summary = pa.get("summary").and_then(|v| v.as_str()).unwrap_or("an action");
        let _ = writeln!(
            out,
            "\n⏸ Needs your approval before it happens: {summary}\n   To approve, call ask_business again with approve=true and conversation_id=\"{cid}\" (or approve=false to decline)."
        );
    }
    if !cid.is_empty() {
        let _ = writeln!(out, "\n[conversation_id: {cid}]");
    }
    out.trim().to_string()
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

    /// The turn renderer surfaces reply, tool calls, a pending-action approval
    /// prompt, and the conversation id — the safety-critical "never send without
    /// approval" path is a pure function, so test it directly.
    #[test]
    fn render_turn_surfaces_pending_action_and_conversation() {
        let turn = serde_json::json!({
            "conversationId": "c-42",
            "reply": "Drafted the renewal.",
            "toolCalls": [{ "name": "knowledge.search" }, { "name": "email.draft" }],
            "pendingAction": { "name": "email.send", "summary": "send the renewal to Acme" }
        });
        let out = render_turn(&turn);
        assert!(out.contains("Drafted the renewal."));
        assert!(out.contains("knowledge.search") && out.contains("email.draft"));
        assert!(out.contains("Needs your approval") && out.contains("send the renewal to Acme"));
        assert!(out.contains("approve=true") && out.contains("c-42"));
        assert!(out.contains("[conversation_id: c-42]"));
    }

    #[test]
    fn render_turn_plain_reply_has_no_approval_prompt() {
        let turn = serde_json::json!({ "conversationId": "c-1", "reply": "Revenue is up 12%." });
        let out = render_turn(&turn);
        assert!(out.contains("Revenue is up 12%."));
        assert!(!out.contains("Needs your approval"));
        assert!(out.contains("[conversation_id: c-1]"));
    }
}
