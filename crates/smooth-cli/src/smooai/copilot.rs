//! `th api copilot …` — drive the org's always-on dashboard Copilot from
//! the CLI. Authenticates as the logged-in *user* (`th auth login`), same
//! as `th api crm`, because the copilot routes are `auth: 'user'` (they
//! 401 under an M2M client) — every tool run is audit-logged against the
//! real person. Pearl th-f15107; smooai PR #2383.
//!
//! Three subcommands mirror the three routes:
//!   chat     POST /organizations/{org}/copilot/chat      {message, conversationId?}
//!   confirm  POST /organizations/{org}/copilot/confirm   {conversationId, approve}
//!   history  GET  /organizations/{org}/copilot/conversations/{id}
//!
//! Responses are buffered JSON (`CopilotTurnResult`) — token streaming is
//! phase 2 on the smooai side. A turn may return a `pendingAction`: a
//! destructive tool (e.g. `email.send`) the loop paused on. `chat` resolves
//! it inline — a y/N prompt on a TTY, or the up-front `--confirm`/
//! `--no-confirm` flag for non-interactive/agent use. `--no-confirm` is
//! never a default: without a flag on a non-TTY we print the pending action
//! and stop rather than silently approving or declining.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde::Deserialize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Send a message to the org copilot and print its reply. Resolves any
    /// destructive-action confirmation inline (TTY prompt or `--confirm`/`--no-confirm`).
    Chat {
        /// The message to send to the copilot.
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
    /// Approve or decline the destructive action a prior `chat` turn paused on,
    /// without resending the message. Use this to inspect first, then decide.
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
    /// Print the message history of a copilot conversation.
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

// ---- API response shapes (subset of smooai CopilotTurnResult / CopilotHistory) ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnResult {
    conversation_id: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pending_action: Option<PendingAction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCall {
    name: String,
    /// `ran` | `pending` | `error`.
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingAction {
    name: String,
    #[serde(default)]
    summary: String,
}

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

/// What to do with a `pendingAction`, given the flags and whether we're on a TTY.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Approve,
    Decline,
    /// Ask the user interactively (TTY only).
    Prompt,
    /// Non-TTY with no flag — refuse to guess; print the action and stop.
    NeedFlag,
}

/// Pure confirm-flag resolution. clap already rejects `--confirm --no-confirm`
/// together, so the `(true, true)` arm can't be reached in practice.
fn decide(confirm: bool, no_confirm: bool, is_tty: bool) -> Decision {
    match (confirm, no_confirm) {
        (true, _) => Decision::Approve,
        (_, true) => Decision::Decline,
        _ if is_tty => Decision::Prompt,
        _ => Decision::NeedFlag,
    }
}

/// One compact line per tool call, e.g. `ran crm.search_contacts`. The status
/// (`ran`/`pending`/`error`) reads as the verb. Glyph-free so it's cheap to
/// test; the print path colors the leading verb.
fn tool_call_line(tc: &ToolCall) -> String {
    format!("{} {}", tc.status, tc.name)
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
        } => chat(&client, resolve_org(org)?, message, conversation, confirm, no_confirm, json).await,
        Cmd::Confirm {
            conversation_id,
            approve,
            decline,
            org,
            json,
        } => {
            if !approve && !decline {
                anyhow::bail!("pass `--approve` or `--decline`");
            }
            confirm(&client, &resolve_org(org)?, &conversation_id, approve, json).await
        }
        Cmd::History { conversation_id, org, json } => history(&client, &resolve_org(org)?, &conversation_id, json).await,
    }
}

async fn chat(client: &UserClient, org: String, message: String, conversation: Option<String>, confirm_flag: bool, no_confirm: bool, json: bool) -> Result<()> {
    let mut body = json!({ "message": message });
    if let Some(cid) = conversation.filter(|s| !s.trim().is_empty()) {
        body["conversationId"] = Value::String(cid);
    }
    let raw = client
        .post(&format!("/organizations/{org}/copilot/chat"), &body)
        .await
        .context("POST copilot chat")?;
    resolve_turn(client, &org, raw, confirm_flag, no_confirm, json).await
}

async fn confirm(client: &UserClient, org: &str, conversation_id: &str, approve: bool, json: bool) -> Result<()> {
    let raw = post_confirm(client, org, conversation_id, approve).await?;
    if json {
        print_json(&raw);
        return Ok(());
    }
    render_turn(&parse_turn(&raw)?);
    Ok(())
}

async fn post_confirm(client: &UserClient, org: &str, conversation_id: &str, approve: bool) -> Result<Value> {
    let body = json!({ "conversationId": conversation_id, "approve": approve });
    client
        .post(&format!("/organizations/{org}/copilot/confirm"), &body)
        .await
        .context("POST copilot confirm")
}

/// Render a turn, then keep resolving any `pendingAction` (a turn can pause on
/// several destructive tools) until the loop settles or we hit a non-TTY with
/// no decision flag.
async fn resolve_turn(client: &UserClient, org: &str, raw: Value, confirm_flag: bool, no_confirm: bool, json: bool) -> Result<()> {
    if json {
        print_json(&raw);
        // In JSON mode we don't auto-confirm — the caller inspects the
        // pendingAction and drives `confirm` itself.
        return Ok(());
    }
    let mut turn = parse_turn(&raw)?;
    loop {
        render_turn(&turn);
        let Some(pending) = turn.pending_action.as_ref() else { return Ok(()) };
        let approve = match decide(confirm_flag, no_confirm, std::io::stdin().is_terminal()) {
            Decision::Approve => true,
            Decision::Decline => false,
            Decision::Prompt => prompt_confirm(pending)?,
            Decision::NeedFlag => {
                println!(
                    "  {} pending action needs a decision — re-run with {} or {}, or `th api copilot confirm {} --approve|--decline`",
                    "!".yellow().bold(),
                    "--confirm".bold(),
                    "--no-confirm".bold(),
                    turn.conversation_id.cyan(),
                );
                return Ok(());
            }
        };
        let raw = post_confirm(client, org, &turn.conversation_id, approve).await?;
        turn = parse_turn(&raw)?;
    }
}

fn prompt_confirm(pending: &PendingAction) -> Result<bool> {
    dialoguer::Confirm::new()
        .with_prompt(format!("Approve {} — {}?", pending.name, pending.summary))
        .default(false)
        .interact()
        .context("read confirmation")
}

fn parse_turn(raw: &Value) -> Result<TurnResult> {
    serde_json::from_value(raw.clone()).context("parse copilot turn result")
}

fn render_turn(turn: &TurnResult) {
    println!();
    for tc in &turn.tool_calls {
        let line = tool_call_line(tc);
        let glyph = match tc.status.as_str() {
            "error" => "✗".red().to_string(),
            "pending" => "…".yellow().to_string(),
            _ => "✓".green().to_string(),
        };
        println!("  {} {}", glyph, line.dimmed());
    }
    if !turn.reply.trim().is_empty() {
        if !turn.tool_calls.is_empty() {
            println!();
        }
        println!("{}", turn.reply);
    }
    if let Some(p) = &turn.pending_action {
        println!();
        println!("  {} {} — {}", "⚠".yellow().bold(), p.name.bold(), p.summary);
    }
    println!();
}

async fn history(client: &UserClient, org: &str, conversation_id: &str, json: bool) -> Result<()> {
    let raw = client
        .get(&format!("/organizations/{org}/copilot/conversations/{conversation_id}"))
        .await
        .context("GET copilot conversation")?;
    if json {
        print_json(&raw);
        return Ok(());
    }
    let hist: History = serde_json::from_value(raw).context("parse copilot history")?;
    println!();
    if hist.messages.is_empty() {
        println!("  {} {}", "●".dimmed(), "no messages".dimmed());
        println!();
        return Ok(());
    }
    for m in &hist.messages {
        let who = match m.role.as_str() {
            "user" => "you".cyan().to_string(),
            "assistant" => "copilot".green().to_string(),
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
    use super::{decide, resolve_org, tool_call_line, Decision, ToolCall};

    fn tc(name: &str, status: &str) -> ToolCall {
        ToolCall {
            name: name.into(),
            status: status.into(),
        }
    }

    #[test]
    fn confirm_flag_wins_over_tty() {
        assert_eq!(decide(true, false, true), Decision::Approve);
        assert_eq!(decide(true, false, false), Decision::Approve);
    }

    #[test]
    fn no_confirm_flag_declines() {
        assert_eq!(decide(false, true, true), Decision::Decline);
        assert_eq!(decide(false, true, false), Decision::Decline);
    }

    #[test]
    fn tty_with_no_flag_prompts() {
        assert_eq!(decide(false, false, true), Decision::Prompt);
    }

    #[test]
    fn non_tty_with_no_flag_refuses_to_guess() {
        // The safety guarantee: never auto-approve or auto-decline for an agent.
        assert_eq!(decide(false, false, false), Decision::NeedFlag);
    }

    #[test]
    fn tool_call_line_is_compact_verb_plus_name() {
        assert_eq!(tool_call_line(&tc("crm.search_contacts", "ran")), "ran crm.search_contacts");
        assert_eq!(tool_call_line(&tc("email.send", "pending")), "pending email.send");
        assert_eq!(tool_call_line(&tc("crm.create_contact", "error")), "error crm.create_contact");
        // Unknown status is passed through, not dropped.
        assert_eq!(tool_call_line(&tc("x.y", "weird")), "weird x.y");
    }

    #[test]
    fn resolve_org_prefers_flag_then_env() {
        assert_eq!(resolve_org(Some("org-flag".into())).unwrap(), "org-flag");
        assert!(resolve_org(Some("   ".into())).is_err() || resolve_org(None).is_err());
    }

    #[test]
    fn parse_turn_reads_camelcase_and_defaults() {
        // Full shape.
        let full = serde_json::json!({
            "conversationId": "c1",
            "reply": "found 3",
            "toolCalls": [{ "name": "crm.search_contacts", "input": {}, "status": "ran" }],
            "pendingAction": { "toolCallId": "t1", "name": "email.send", "input": {}, "summary": "send to jane" }
        });
        let t = super::parse_turn(&full).unwrap();
        assert_eq!(t.conversation_id, "c1");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.pending_action.unwrap().name, "email.send");

        // Minimal shape — only conversationId; the rest default.
        let min = serde_json::json!({ "conversationId": "c2" });
        let t = super::parse_turn(&min).unwrap();
        assert_eq!(t.conversation_id, "c2");
        assert!(t.reply.is_empty());
        assert!(t.tool_calls.is_empty());
        assert!(t.pending_action.is_none());
    }
}
