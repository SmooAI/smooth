//! `th api smooth-operator …` — drive the org's always-on dashboard Smooth Operator from
//! the CLI. Authenticates as the logged-in *user* (`th auth login`), same
//! as `th api crm`, because the smooth-operator routes are `auth: 'user'` (they
//! 401 under an M2M client) — every tool run is audit-logged against the
//! real person. Pearl th-f15107; smooai PR #2383.
//!
//! **Transport (SMOODEV-2673):** the buffered REST chat/confirm routes were
//! deleted; the operator is now driven over its **SEP WebSocket**. `chat` mints
//! a short-lived socket token and runs one turn via
//! [`crate::smooai::smooth_operator_ws::operator_turn`]. Destructive tools park
//! the turn mid-flight and are confirmed **inline** on the same socket, so
//! approval is a flag on `chat` (`--confirm`) rather than a follow-up command —
//! the old `confirm` subcommand is retired and now explains the change.
//! Without `--confirm`, destructive actions are declined and reported, never
//! silently run.
//!
//!   chat     SEP WS  wss://smooth-operator.smoo.ai/ws  (token from api-prime)
//!   history  GET     /organizations/{org}/smooth-operator/conversations/{id}

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde::Deserialize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Send a message to the org smooth-operator and print its reply. Runs one
    /// turn over the SEP WebSocket; destructive actions run only with `--confirm`.
    Chat {
        /// The message to send to the smooth-operator.
        message: String,
        /// Continue an existing conversation. Omit to start a new one.
        #[arg(long = "conversation", visible_alias = "conversation-id")]
        conversation: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Auto-approve any destructive action this turn triggers (non-interactive).
        #[arg(long, conflicts_with = "no_confirm")]
        confirm: bool,
        /// Auto-decline any destructive action this turn triggers (non-interactive).
        #[arg(long = "no-confirm")]
        no_confirm: bool,
        /// Print the raw JSON turn result instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// RETIRED — the operator now confirms destructive actions inline during the
    /// turn. Use `chat --confirm` instead; this command explains the change.
    Confirm {
        /// The conversation id from the `chat` turn that returned a pendingAction.
        conversation_id: String,
        /// Approve the pending action (runs it). Mutually exclusive with `--decline`.
        #[arg(long, conflicts_with = "decline")]
        approve: bool,
        /// Decline the pending action (drops it).
        #[arg(long)]
        decline: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON turn result instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// List or configure which tools the org's Smooth Operator may use.
    /// **Org admin only** — this decides what your AI is allowed to do.
    Tools {
        #[command(subcommand)]
        cmd: ToolsCmd,
    },
    /// Print the message history of a smooth-operator conversation.
    History {
        /// The conversation id from a `chat` turn.
        conversation_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ToolsCmd {
    /// Show the operator's tool catalog and which are enabled for your org.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the rendered view.
        #[arg(long)]
        json: bool,
    },
    /// Turn a tool ON for the org (e.g. `email.send`).
    Enable {
        /// Dotted tool id, as shown by `tools list`.
        tool_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Turn a tool OFF for the org — the operator can no longer use it at all.
    Disable {
        /// Dotted tool id, as shown by `tools list`.
        tool_id: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

// ---- API response shapes (subset of smooai SmoothOperatorHistory) ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct History {
    #[serde(default)]
    messages: Vec<HistoryMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_name: Option<String>,
}

fn resolve_org(override_org: Option<String>) -> Result<String> {
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    anyhow::bail!("no org specified — pass `--org <id>` or set SMOOAI_ORG_ID")
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::Chat {
            message,
            conversation,
            org,
            confirm,
            no_confirm,
            json,
        } => chat(resolve_org(org)?, message, conversation, confirm, no_confirm, json).await,
        // The separate confirm round-trip is obsolete: the SEP WebSocket parks
        // the turn and takes the approval inline, so approval is a flag on
        // `chat` now rather than a follow-up command.
        Cmd::Confirm { .. } => anyhow::bail!(
            "`confirm` is obsolete — the operator now confirms destructive actions inline over its WebSocket transport. \
             Re-run the ask with `th api smooth-operator chat \"…\" --confirm` to approve actions during the turn."
        ),
        Cmd::Tools { cmd } => match cmd {
            ToolsCmd::List { org, json } => tools_list(&resolve_org(org)?, json).await,
            ToolsCmd::Enable { tool_id, org } => tools_set(&resolve_org(org)?, &tool_id, true).await,
            ToolsCmd::Disable { tool_id, org } => tools_set(&resolve_org(org)?, &tool_id, false).await,
        },
        Cmd::History { conversation_id, org, json } => history(&client, &resolve_org(org)?, &conversation_id, json).await,
    }
}

/// One operator turn over the SEP WebSocket. The buffered REST chat route was
/// deleted in SMOODEV-2673; `operator_turn` mints the socket token and drives
/// the session. `--confirm` approves destructive tools inline; otherwise they
/// are declined and reported.
async fn chat(org: String, message: String, conversation: Option<String>, confirm_flag: bool, no_confirm: bool, json: bool) -> Result<()> {
    let approve = confirm_flag && !no_confirm;
    let turn = crate::smooai::smooth_operator_ws::operator_turn(&org, &message, conversation.as_deref().filter(|s| !s.trim().is_empty()), approve)
        .await
        .context("smooth-operator turn")?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "reply": turn.reply,
                "conversationId": turn.conversation_id,
                "declined": turn.declined,
            }))?
        );
    } else {
        println!("{}", crate::smooai::smooth_operator_ws::render_operator_turn(&turn));
    }
    Ok(())
}

async fn history(client: &UserClient, org: &str, conversation_id: &str, json: bool) -> Result<()> {
    let raw = client
        .get(&format!("/organizations/{org}/smooth-operator/conversations/{conversation_id}"))
        .await
        .context("GET smooth-operator conversation")?;
    if json {
        print_json(&raw);
        return Ok(());
    }
    let hist: History = serde_json::from_value(raw).context("parse smooth-operator history")?;
    println!();
    if hist.messages.is_empty() {
        println!("  {} {}", "●".dimmed(), "no messages".dimmed());
        println!();
        return Ok(());
    }
    for m in &hist.messages {
        let who = match m.role.as_str() {
            "user" => "you".cyan().to_string(),
            "assistant" => "smooth-operator".green().to_string(),
            "tool" => m.tool_name.clone().unwrap_or_else(|| "tool".into()).yellow().to_string(),
            other => other.to_string(),
        };
        println!("  {} {}", who, m.content);
    }
    println!();
    Ok(())
}

// ---- Operator tool config (per-org: which tools the operator may use) -------

/// One entry of the operator's tool catalog, as returned by
/// `GET /organizations/{org}/smooth-operator/tools`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorTool {
    /// Dotted tool name (`email.send`) — the id a write references.
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Requires human approval before it runs.
    #[serde(default)]
    pub destructive: bool,
    /// The ORG's config: is this tool turned on? (Default on.)
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whether the requesting user's feature entitlements + permissions would
    /// expose it at all. Effective exposure = `available && enabled`.
    #[serde(default)]
    pub available: bool,
}

const fn yes() -> bool {
    true
}

fn parse_catalog(raw: &Value) -> Result<Vec<OperatorTool>> {
    serde_json::from_value(raw.get("tools").cloned().unwrap_or(Value::Null)).context("parse smooth-operator tool catalog")
}

/// The org's operator tool catalog + current enabled state. Admin-only server-side.
///
/// # Errors
/// Errors without a Smoo user session, if the caller isn't an org admin (403),
/// or if the request fails.
pub async fn list_operator_tools(org: &str) -> Result<Vec<OperatorTool>> {
    let client = UserClient::from_user_session().await?;
    let raw = client
        .get(&format!("/organizations/{org}/smooth-operator/tools"))
        .await
        .context("GET smooth-operator tools")?;
    parse_catalog(&raw)
}

/// Turn one tool on/off for the org, returning the authoritative new catalog.
///
/// **Read-modify-write, deliberately.** The PUT body is authoritative: the
/// server persists only the disabled subset, so any tool *omitted* from the body
/// is treated as ENABLED. Sending a one-entry body would therefore silently
/// re-enable every other tool. We re-read the full catalog, flip just this one,
/// and send all of it.
///
/// # Errors
/// Errors on an unknown `tool_id`, without a Smoo user session, if the caller
/// isn't an org admin (403), or if a request fails.
pub async fn set_operator_tool(org: &str, tool_id: &str, enabled: bool) -> Result<Vec<OperatorTool>> {
    let mut tools = list_operator_tools(org).await?;
    if !tools.iter().any(|t| t.id == tool_id) {
        anyhow::bail!("unknown tool `{tool_id}` — run `th api smooth-operator tools list` to see the catalog");
    }
    for t in &mut tools {
        if t.id == tool_id {
            t.enabled = enabled;
        }
    }
    let body = json!({
        "tools": tools.iter().map(|t| json!({ "toolId": t.id, "enabled": t.enabled })).collect::<Vec<_>>(),
    });
    let client = UserClient::from_user_session().await?;
    let raw = client
        .put(&format!("/organizations/{org}/smooth-operator/tools"), &body)
        .await
        .context("PUT smooth-operator tools")?;
    parse_catalog(&raw)
}

/// First sentence/line of a tool description, capped — enough to explain the
/// tool, short enough that a 45-tool catalog stays readable. Empty in, empty out.
fn summarize(description: &str) -> String {
    let first = description.split(['\n', '.']).map(str::trim).find(|s| !s.is_empty()).unwrap_or_default();
    if first.is_empty() {
        return String::new();
    }
    let mut s: String = first.chars().take(70).collect();
    if first.chars().count() > 70 {
        s.push('…');
    }
    format!(" — {s}")
}

/// Render the catalog as compact text (shared by the CLI and the MCP tool).
#[must_use]
pub fn render_tool_catalog(tools: &[OperatorTool]) -> String {
    use std::fmt::Write as _;
    let (on, off) = tools.iter().partition::<Vec<_>, _>(|t| t.enabled);
    let mut out = format!("{} tool(s): {} enabled, {} disabled\n", tools.len(), on.len(), off.len());
    for t in tools {
        let state = if t.enabled { "on " } else { "OFF" };
        let flags = match (t.destructive, t.available) {
            (true, true) => " [needs approval]",
            (true, false) => " [needs approval; unavailable to you]",
            (false, false) => " [unavailable to you]",
            (false, true) => "",
        };
        // A one-line gist so an MCP client can explain the tool without a
        // second lookup; the full text is often a paragraph.
        let gist = summarize(&t.description);
        let _ = writeln!(out, "  {state}  {}{flags}{gist}", t.id);
    }
    out.trim_end().to_string()
}

async fn tools_list(org: &str, json_out: bool) -> Result<()> {
    let tools = list_operator_tools(org).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "tools": tools.iter().map(|t| json!({"id": t.id, "enabled": t.enabled, "destructive": t.destructive, "available": t.available})).collect::<Vec<_>>() })
            )?
        );
    } else {
        println!("{}", render_tool_catalog(&tools));
    }
    Ok(())
}

async fn tools_set(org: &str, tool_id: &str, enabled: bool) -> Result<()> {
    let tools = set_operator_tool(org, tool_id, enabled).await?;
    let verb = if enabled {
        "enabled".green().to_string()
    } else {
        "disabled".yellow().to_string()
    };
    println!("{verb} {}", tool_id.bold());
    println!("{}", render_tool_catalog(&tools));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_catalog, render_tool_catalog, resolve_org};
    use serde_json::json;

    #[test]
    fn resolve_org_prefers_flag_then_env() {
        assert_eq!(resolve_org(Some("org-flag".into())).unwrap(), "org-flag");
        assert!(resolve_org(Some("   ".into())).is_err() || resolve_org(None).is_err());
    }

    #[test]
    fn parse_catalog_defaults_enabled_true() {
        // `enabled` absent must mean ON — an absent org config means every tool
        // is enabled, so defaulting to false would read as "everything is off".
        let raw = json!({ "tools": [{ "id": "email.send" }] });
        let tools = parse_catalog(&raw).unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].enabled, "missing `enabled` must default to true");
    }

    #[test]
    fn render_marks_disabled_and_destructive() {
        let raw = json!({ "tools": [
            { "id": "crm.search_contacts", "enabled": true, "destructive": false, "available": true },
            { "id": "email.send", "enabled": false, "destructive": true, "available": true },
        ]});
        let out = render_tool_catalog(&parse_catalog(&raw).unwrap());
        assert!(out.contains("2 tool(s): 1 enabled, 1 disabled"));
        assert!(out.contains("on   crm.search_contacts"));
        assert!(out.contains("OFF  email.send [needs approval]"));
    }
}
