//! `th smoo billing …` — what the org is on (SMOODEV-3609).
//!
//! `plan` reads `GET /organizations/{org}/billing/plan-usage`: the active plans
//! (with seat counts and each plan's limits), the included usage allowances
//! (AI conversations, voice and per-seat calling minutes, meeting-bot minutes,
//! HeyPage Studio credits — SMOODEV-3567) and the enabled features. CLI twin of
//! the Smooth Operator's `billing.plan_usage` and the hosted MCP's
//! `billing_plan_usage`; needs the "View the plan, features & usage"
//! (`billing.read`) permission. Read-only — no prices or payment details.

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::Value;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the org's active plans, their limits, included allowances and enabled features.
    #[command(visible_alias = "usage")]
    Plan {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Plan { org, json } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/billing/plan-usage"))
                .await
                .context("GET billing/plan-usage (needs the billing.read permission)")?;
            if json {
                print_json(&body);
            } else {
                for line in render(&body) {
                    println!("{line}");
                }
            }
        }
    }
    Ok(())
}

/// Labels for the allowance keys the route sums across plans.
const ALLOWANCES: &[(&str, &str)] = &[
    ("conversations", "AI conversations / month"),
    ("voiceMinutes", "voice minutes / month"),
    ("callingMinutes", "calling minutes / month (pooled, human talk time)"),
    ("botMinutes", "meeting-bot minutes / month"),
    ("heypageCredits", "HeyPage Studio credits / month"),
];

/// The human summary, one line per entry.
fn render(body: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let products = body.get("products").and_then(Value::as_array).cloned().unwrap_or_default();
    if products.is_empty() {
        out.push("No paid plans — free tier.".to_string());
    } else {
        out.push(format!("{}", "Plans".bold()));
        for p in &products {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
            let seats = p
                .get("seats")
                .and_then(Value::as_i64)
                .filter(|n| *n > 1)
                .map_or(String::new(), |n| format!(" ({n} seats)"));
            out.push(format!("  {name}{seats}"));
            if let Some(q) = p.get("quotas").and_then(Value::as_object) {
                let mut keys: Vec<_> = q.iter().collect();
                keys.sort_by(|a, b| a.0.cmp(b.0));
                for (k, v) in keys {
                    out.push(format!("      {} {v}", format!("{k}:").dimmed()));
                }
            }
        }
    }
    if let Some(a) = body.get("allowances").and_then(Value::as_object) {
        let lines: Vec<String> = ALLOWANCES
            .iter()
            .filter_map(|(k, label)| a.get(*k).and_then(Value::as_i64).map(|n| format!("  {n} {label}")))
            .collect();
        if !lines.is_empty() {
            out.push(format!("{}", "Included".bold()));
            out.extend(lines);
        }
    }
    if let Some(rate) = body.pointer("/callingAllowance/overageRateCents").and_then(Value::as_i64) {
        out.push(format!("  calling overage: {rate}¢/minute"));
    }
    let features: Vec<&str> = body
        .get("features")
        .and_then(Value::as_array)
        .map(|f| f.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    out.push(format!(
        "{} {}",
        "Features:".bold(),
        if features.is_empty() { "none".to_string() } else { features.join(", ") }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(lines: Vec<String>) -> String {
        lines.join("\n")
    }

    #[test]
    fn renders_plans_allowances_and_features() {
        let out = plain(render(&json!({
            "products": [{ "name": "Smoo Calling", "seats": 3, "quotas": { "includedCallingMinutesPerSeat": 300 } }],
            "features": ["telephony"],
            "allowances": { "callingMinutes": 900, "botMinutes": null },
            "callingAllowance": { "overageRateCents": 5 }
        })));
        assert!(out.contains("Smoo Calling (3 seats)"), "{out}");
        assert!(out.contains("900 calling minutes"), "{out}");
        assert!(!out.contains("meeting-bot"), "absent allowance hidden: {out}");
        assert!(out.contains("5¢/minute"));
        assert!(out.contains("telephony"));
    }

    #[test]
    fn free_tier_says_so() {
        let out = plain(render(&json!({ "products": [], "features": [] })));
        assert!(out.contains("free tier") && out.contains("none"), "{out}");
    }
}
