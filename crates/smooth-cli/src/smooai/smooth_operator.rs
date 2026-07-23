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

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde::Deserialize;
use serde_json::json;

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

#[cfg(test)]
mod tests {
    use super::resolve_org;

    #[test]
    fn resolve_org_prefers_flag_then_env() {
        assert_eq!(resolve_org(Some("org-flag".into())).unwrap(), "org-flag");
        assert!(resolve_org(Some("   ".into())).is_err() || resolve_org(None).is_err());
    }
}
