//! `th smoo inbox` — the org's recent customer conversations (SMOODEV-3609).
//!
//! Reads `GET /organizations/{org}/conversations` (newest first), optionally only
//! the ones with an open escalation — the "needs a human" inbox — or one agent's.
//! CLI twin of the Smooth Operator's `inbox.recent_conversations` and the hosted
//! MCP's `inbox_recent_conversations`. Needs the "View conversations &
//! escalations" (`conversations.read`) permission and a signed-in user (the
//! route refuses an org API key).

use anstream::println;
use anyhow::{Context, Result};
use clap::Args;
use owo_colors::OwoColorize;
use serde_json::Value;

use super::{print_json, require_active_org, require_authed};

#[derive(Args)]
pub struct InboxArgs {
    /// Only conversations with an open escalation.
    #[arg(long)]
    pub escalated: bool,
    /// Only conversations served by this agent (id from `th smoo agents activity`).
    #[arg(long = "agent-id", value_name = "AGENT_ID")]
    pub agent_id: Option<String>,
    /// Max conversations (1-100, default 10).
    #[arg(long, default_value_t = 10)]
    pub limit: u32,
    /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
    #[arg(long = "org-id", visible_alias = "org")]
    pub org: Option<String>,
    /// Print raw JSON instead of the list.
    #[arg(long)]
    pub json: bool,
}

/// The query string for the list route.
fn query(args: &InboxArgs) -> String {
    let mut q = vec![format!("limit={}", args.limit.clamp(1, 100)), "sort=newest".to_string()];
    if args.escalated {
        q.push("escalated=true".to_string());
    }
    if let Some(a) = args.agent_id.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        q.push(format!("agentId={}", urlencoding::encode(a)));
    }
    q.join("&")
}

pub async fn cmd(args: InboxArgs) -> Result<()> {
    let client = require_authed().await?;
    let o = require_active_org(&client, args.org.clone())?;
    let body = client
        .get(&format!("/organizations/{o}/conversations?{}", query(&args)))
        .await
        .context("GET conversations (needs conversations.read and a signed-in user)")?;
    if args.json {
        print_json(&body);
        return Ok(());
    }
    let rows = body.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("{}", if args.escalated { "No open escalations." } else { "No conversations." });
        return Ok(());
    }
    for c in &rows {
        println!("{}", line(c));
    }
    if body.pointer("/pagination/hasMore").and_then(Value::as_bool).unwrap_or(false) {
        println!("{}", "(more exist — raise --limit)".dimmed());
    }
    Ok(())
}

fn line(c: &Value) -> String {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
    let who = c
        .get("contact")
        .and_then(|ct| ct.get("name").or_else(|| ct.get("email")))
        .and_then(Value::as_str)
        .map_or(String::new(), |w| format!(" — {w}"));
    let updated = if s("updatedAt").is_empty() { s("createdAt") } else { s("updatedAt") };
    format!("[{}] {}{who}  {}  {}", s("platform"), s("name"), updated, s("id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: InboxArgs,
    }

    #[test]
    fn query_carries_the_filters_and_clamps_the_limit() {
        let w = Wrap::try_parse_from(["t", "--escalated", "--agent-id", "a-1", "--limit", "999"]).unwrap();
        assert_eq!(query(&w.args), "limit=100&sort=newest&escalated=true&agentId=a-1");
        let w = Wrap::try_parse_from(["t"]).unwrap();
        assert_eq!(query(&w.args), "limit=10&sort=newest");
    }

    #[test]
    fn a_row_names_the_platform_customer_and_id() {
        let l = line(&json!({ "id": "c-1", "platform": "sms", "name": "Billing", "updatedAt": "2026-10-02",
                              "contact": { "name": "Ada" } }));
        assert_eq!(l, "[sms] Billing — Ada  2026-10-02  c-1");
    }
}
