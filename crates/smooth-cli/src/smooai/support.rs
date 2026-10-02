//! `th smoo support …` — the CLI twin of the copilot's `support.report` and the
//! hosted MCP `support_report` (SMOODEV-3611).
//!
//! `report` reads `GET /organizations/{org}/support/reports`, the same
//! pre-aggregated stats the dashboard's Support → Reports page renders (open /
//! solved, CSAT, first-response time, SLA breach rate, volume by status and
//! channel). The route is user-only and gated on `support.read` + the
//! `support` product, so this needs a signed-in session (`smoo auth login`).

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::Value;

use super::{print_json, require_active_org, require_user_session};

#[derive(Subcommand)]
pub enum Cmd {
    /// Support health: open/solved tickets, CSAT, average first response, SLA
    /// breach rate, and volume by status/channel.
    #[command(visible_alias = "reports")]
    Report {
        /// Print the raw response JSON.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Report { json, org } => {
            let client = require_user_session()
                .await
                .context("support reports need a user session — run `smoo auth login`")?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/support/reports"))
                .await
                .context("GET support/reports")?;
            if json {
                print_json(&body);
            } else {
                print!("{}", render_report(&body));
            }
        }
    }
    Ok(())
}

/// `firstResponseTime` → `first response time`.
fn label(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push(' ');
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("—".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(match n.as_f64() {
            Some(f) if f.fract() != 0.0 => format!("{f:.2}"),
            _ => n.to_string(),
        }),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Render the aggregate generically: scalars as `label: value`, nested objects
/// (by-status, by-channel) as an indented breakdown, arrays of `{name/key,
/// count}` rows likewise. Generic on purpose — the stats shape grows, and a
/// hard-coded field list would silently drop the new numbers.
pub(crate) fn render_report(body: &Value) -> String {
    let mut out = String::new();
    let Some(obj) = body.as_object() else {
        return format!("{body}\n");
    };
    out.push_str(&format!("  {}\n", "Support report".bold()));
    for (k, v) in obj {
        if let Some(s) = scalar(v) {
            out.push_str(&format!("  {:<28} {s}\n", label(k)));
        }
    }
    for (k, v) in obj {
        match v {
            Value::Object(m) => {
                out.push_str(&format!("  {}\n", label(k).bold()));
                for (sk, sv) in m {
                    let s = scalar(sv).unwrap_or_else(|| sv.to_string());
                    out.push_str(&format!("    {:<26} {s}\n", label(sk)));
                }
            }
            Value::Array(rows) => {
                out.push_str(&format!("  {}\n", label(k).bold()));
                for row in rows {
                    let name = ["name", "key", "status", "channel", "label"]
                        .iter()
                        .find_map(|f| row.get(*f).and_then(scalar))
                        .unwrap_or_else(|| "?".to_string());
                    let count = ["count", "value", "total"]
                        .iter()
                        .find_map(|f| row.get(*f).and_then(scalar))
                        .unwrap_or_else(|| row.to_string());
                    out.push_str(&format!("    {name:<26} {count}\n"));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn labels_split_camel_and_snake_case() {
        assert_eq!(label("avgFirstResponseMinutes"), "avg first response minutes");
        assert_eq!(label("sla_breach_rate"), "sla breach rate");
    }

    #[test]
    fn report_renders_scalars_breakdowns_and_rows() {
        let body = json!({
            "open": 4,
            "csat": 4.256,
            "slaBreachRate": null,
            "byStatus": { "open": 4, "solved": 9 },
            "byChannel": [{ "channel": "email", "count": 7 }],
        });
        let out = render_report(&body);
        assert!(out.contains("open") && out.contains(" 4\n"));
        assert!(out.contains("4.26"), "floats are rounded: {out}");
        assert!(out.contains("sla breach rate") && out.contains("—"));
        assert!(out.contains("by status") && out.contains("solved"));
        assert!(out.contains("by channel") && out.contains("email") && out.contains(" 7\n"));
    }
}
