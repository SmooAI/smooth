//! `th notify …` — ping the human on their own phone.
//!
//! This is an **agentic primitive**: an AI agent running under `th`
//! (Big Smooth, the claude-driver) calls `th notify "message"` to reach
//! the operator on their phone — "blocked, need input", "done, review
//! this", "approve this gate". It sends a PUSH + in-app notification to
//! the LOGGED-IN user's OWN devices via `api.smoo.ai`, authenticated as
//! the user (Supabase JWT), so there's no target to address: the human
//! behind the session is the recipient.
//!
//! Wraps `POST /organizations/{org_id}/notifications/self`. Org-scoped
//! (defaults to the active org; a master admin can target a child org
//! with `--org-id`).

use anstream::println;
use anyhow::{Context, Result};
use clap::ValueEnum;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use crate::smooai::user_client::UserClient;

/// Notification urgency. Maps 1:1 to the API's `priority` field; the
/// backend decides how each level surfaces on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

impl Priority {
    fn api_value(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
}

/// Send the notification. `message` is the positional words joined with
/// spaces (so `th notify hello world` works unquoted, like an agent
/// would call it).
pub async fn cmd(message: Vec<String>, title: String, priority: Priority, url: Option<String>, org_id: Option<String>) -> Result<()> {
    let body = message.join(" ");
    if body.trim().is_empty() {
        anyhow::bail!("nothing to send — pass a message, e.g. `th notify blocked, need input`");
    }

    let client = UserClient::from_user_session().await?;
    let org = crate::active_org::resolve(org_id)?;

    let mut payload = json!({
        "title": title,
        "body": body,
        "priority": priority.api_value(),
    });
    if let Some(deep_link) = url {
        payload["deepLink"] = json!(deep_link);
    }

    let resp = client
        .post(&format!("/organizations/{org}/notifications/self"), &payload)
        .await
        .context("POST notifications/self")?;

    print_result(&resp);
    Ok(())
}

/// Print a friendly confirmation from the `{ notificationId, inApp,
/// pushed }` response. Falls back to raw JSON on an unexpected shape.
fn print_result(resp: &Value) {
    let Some(pushed) = resp.get("pushed").and_then(Value::as_u64) else {
        super::print_json(resp);
        return;
    };
    let who = UserClient::user_label().unwrap_or_else(|| "you".to_string());
    let devices = if pushed == 1 { "device" } else { "devices" };
    println!();
    println!("  {} Notified {} — pushed to {} {}", "✓".green().bold(), who.cyan(), pushed, devices);
    if pushed == 0 {
        println!("    {} no registered devices — open the Smoo AI app and allow notifications", "→".dimmed());
    }
    println!();
}
