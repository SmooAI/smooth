//! `smoo campaigns …` — one-shot email/SMS campaigns: list, delivery
//! analytics, and a preview-first send. CLI twin of the hosted MCP
//! `campaign_*` tools (pearl th-b1f09c).
//!
//! `send` is PREVIEW BY DEFAULT: without `--confirm` it posts the server's
//! dry-run mode (`{"dryRun": true}`), which reports the would-be recipient
//! count and every suppression refusal while sending nothing. Only
//! `--confirm` posts `{"dryRun": false}`. Suppression itself is enforced
//! server-side per recipient at send time — never re-implemented here.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's campaigns, optionally narrowed by type or status.
    List {
        /// Only campaigns of this type, e.g. email, sms, social.
        #[arg(long = "type")]
        campaign_type: Option<String>,
        /// Only campaigns in this status: draft, scheduled, active, paused, completed.
        #[arg(long)]
        status: Option<String>,
        /// Print the raw JSON instead of the compact list.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delivery + engagement metrics for one campaign (sent, delivered, opened, clicked, …).
    Analytics {
        /// The campaign id from `smoo campaigns list`.
        campaign_id: String,
        /// Which delivery surface — must match the campaign's own type (the routes are separate).
        #[arg(long, default_value = "email", value_parser = ["email", "sms"])]
        channel: String,
        /// Print the raw JSON instead of the key: value lines.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Send a campaign to its whole recipient list. PREVIEW by default —
    /// reports recipient count + suppression refusals and sends NOTHING.
    /// Pass --confirm to actually send (no per-contact undo).
    Send {
        /// The campaign id from `smoo campaigns list`.
        campaign_id: String,
        /// Which send surface — must match the campaign's own type.
        #[arg(long, default_value = "email", value_parser = ["email", "sms"])]
        channel: String,
        /// Actually send. Without this flag the server runs a dry run and nothing goes out.
        #[arg(long)]
        confirm: bool,
        /// Print the raw JSON response instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List {
            campaign_type,
            status,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            // The route takes no filters and returns every campaign, so type
            // and status are applied here (case-insensitive) — same as the
            // hosted MCP tool.
            let body = client.get(&format!("/organizations/{o}/campaigns")).await.context("GET campaigns")?;
            let rows: Vec<Value> = body
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| matches_filters(r, campaign_type.as_deref(), status.as_deref()))
                .collect();
            if as_json {
                print_json(&json!({ "data": rows }));
            } else if rows.is_empty() {
                println!("\nNo campaigns matched. This is a confirmed read of the campaign list, not a read failure.\n");
            } else {
                println!();
                for r in &rows {
                    let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let ctype = r.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    let cstatus = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("  {id}  {name}  [{ctype}/{cstatus}]");
                }
                println!("\n{} campaign(s).\n", rows.len());
            }
        }
        Cmd::Analytics {
            campaign_id,
            channel,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!(
                    "/organizations/{o}/{channel}-campaigns/{}/analytics",
                    urlencoding::encode(campaign_id.trim())
                ))
                .await
                .context("GET campaign analytics")?;
            if as_json {
                print_json(&body);
            } else {
                println!("\nDelivery results for {channel} campaign {campaign_id}\n{}\n", render_metrics(&body));
            }
        }
        Cmd::Send {
            campaign_id,
            channel,
            confirm,
            json: as_json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let body = client
                .post(
                    &format!("/organizations/{o}/{channel}-campaigns/{}/send", urlencoding::encode(campaign_id.trim())),
                    Some(&send_payload(confirm)),
                )
                .await
                .context("POST campaign send")?;
            if as_json {
                print_json(&body);
            } else {
                println!("\n{}\n", render_send(&body, &campaign_id, confirm));
            }
        }
    }
    Ok(())
}

/// True when the row passes the optional (case-insensitive) type/status
/// filters. The campaigns route returns everything, so this is where
/// `--type`/`--status` are applied — mirroring the hosted MCP tool.
fn matches_filters(row: &Value, campaign_type: Option<&str>, status: Option<&str>) -> bool {
    let want = |key: &str, filter: Option<&str>| -> bool {
        filter
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .is_none_or(|f| row.get(key).and_then(Value::as_str).is_some_and(|v| v.eq_ignore_ascii_case(f)))
    };
    want("type", campaign_type) && want("status", status)
}

/// The send body: server-side dry run unless the user passed `--confirm`.
/// The preview/commit decision lives in ONE place so the parse test that
/// pins "no --confirm ⇒ dryRun true" is testing the real payload.
fn send_payload(confirm: bool) -> Value {
    json!({ "dryRun": !confirm })
}

/// Flat metrics object as sorted `key: value` lines. Not a fixed key list —
/// email and SMS analytics carry different metric sets.
fn render_metrics(body: &Value) -> String {
    let Some(object) = body.as_object() else {
        return "No delivery metrics were returned for this campaign.".to_string();
    };
    let mut lines: Vec<String> = object
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| format!("{k}: {}", v.as_str().map_or_else(|| v.to_string(), ToString::to_string)))
        .collect();
    if lines.is_empty() {
        return "No delivery metrics recorded for this campaign yet. This is a confirmed read, not a read failure.".to_string();
    }
    lines.sort();
    lines.join("\n")
}

/// Human summary of a send response. Always reports the suppression
/// refusals — they are the compliance answer for why someone was not
/// contacted — exactly like the hosted MCP tool.
fn render_send(body: &Value, campaign_id: &str, confirm: bool) -> String {
    let number = |key: &str| body.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut out = if confirm {
        // The two send paths report differently by design: the synchronous
        // route returns sent/failed, the durable starter 202s with sendingTo.
        if body.get("started").and_then(Value::as_bool) == Some(true) {
            format!("Campaign {campaign_id} started — {} recipient(s) queued for delivery.", number("sendingTo"))
        } else {
            format!("Campaign {campaign_id} sent to {} recipient(s); {} failed.", number("sent"), number("failed"))
        }
    } else {
        format!(
            "PREVIEW ONLY — nothing was sent. Campaign {campaign_id} would be sent to {} recipient(s).",
            number("wouldSend")
        )
    };

    let skipped = body.get("skippedRecipients").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if skipped.is_empty() {
        out.push_str("\nNobody was refused by suppression.");
    } else {
        let reasons = body.get("skippedReasons").map(ToString::to_string).unwrap_or_default();
        let _ = write!(out, "\n\nRefused by the server ({} — {reasons}):", skipped.len());
        for row in &skipped {
            let who = row.get("identifier").and_then(Value::as_str).unwrap_or("?");
            let why = row.get("reason").and_then(Value::as_str).unwrap_or("?");
            let _ = write!(out, "\n- {who}: {why}");
        }
        out.push_str("\n(`unsubscribed` and `opted_out` are suppression rules — they cannot be overridden from here.)");
    }
    if !confirm {
        out.push_str("\n\nTo actually send, confirm the count with the user, then re-run with --confirm.");
    }
    out
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::*;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn list_parses_with_and_without_filters() {
        let bare = Wrap::try_parse_from(["t", "list"]).expect("bare list");
        assert!(matches!(
            bare.cmd,
            Cmd::List {
                campaign_type: None,
                status: None,
                json: false,
                ..
            }
        ));
        let full = Wrap::try_parse_from(["t", "list", "--type", "email", "--status", "draft", "--json"]).expect("filtered list");
        match full.cmd {
            Cmd::List {
                campaign_type, status, json, ..
            } => {
                assert_eq!(campaign_type.as_deref(), Some("email"));
                assert_eq!(status.as_deref(), Some("draft"));
                assert!(json);
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn analytics_parses_and_defaults_to_email() {
        let got = Wrap::try_parse_from(["t", "analytics", "camp-1"]).expect("analytics");
        match got.cmd {
            Cmd::Analytics { campaign_id, channel, .. } => {
                assert_eq!(campaign_id, "camp-1");
                assert_eq!(channel, "email");
            }
            _ => panic!("expected Analytics"),
        }
        assert!(
            Wrap::try_parse_from(["t", "analytics", "camp-1", "--channel", "postal"]).is_err(),
            "unknown channel must be refused"
        );
    }

    /// THE invariant of this module: `send` without `--confirm` parses into
    /// the preview path, and that path posts a server-side dry run.
    #[test]
    fn send_without_confirm_is_preview() {
        let got = Wrap::try_parse_from(["t", "send", "camp-1"]).expect("bare send");
        match got.cmd {
            Cmd::Send { confirm, .. } => {
                assert!(!confirm, "send must default to preview");
                assert_eq!(send_payload(confirm), json!({ "dryRun": true }));
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn send_with_confirm_commits() {
        let got = Wrap::try_parse_from(["t", "send", "camp-1", "--confirm", "--channel", "sms"]).expect("confirmed send");
        match got.cmd {
            Cmd::Send { confirm, channel, .. } => {
                assert!(confirm);
                assert_eq!(channel, "sms");
                assert_eq!(send_payload(confirm), json!({ "dryRun": false }));
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn filters_match_case_insensitively() {
        let row = json!({ "type": "email", "status": "Draft" });
        assert!(matches_filters(&row, None, None));
        assert!(matches_filters(&row, Some("EMAIL"), Some("draft")));
        assert!(matches_filters(&row, Some("  email "), None), "filters are trimmed");
        assert!(!matches_filters(&row, Some("sms"), None));
        assert!(!matches_filters(&row, None, Some("active")));
        // A row missing the field never matches a set filter.
        assert!(!matches_filters(&json!({}), Some("email"), None));
    }

    #[test]
    fn render_send_preview_reports_suppression() {
        let body = json!({
            "wouldSend": 40,
            "skippedRecipients": [{ "identifier": "a@x.com", "reason": "unsubscribed" }],
            "skippedReasons": { "unsubscribed": 1 },
        });
        let out = render_send(&body, "camp-1", false);
        assert!(out.contains("PREVIEW ONLY"), "{out}");
        assert!(out.contains("40 recipient(s)"), "{out}");
        assert!(out.contains("a@x.com: unsubscribed"), "{out}");
        assert!(out.contains("--confirm"), "{out}");
    }

    #[test]
    fn render_send_commit_reports_both_shapes() {
        let sync = render_send(&json!({ "sent": 10, "failed": 2, "skippedRecipients": [] }), "c", true);
        assert!(sync.contains("sent to 10 recipient(s); 2 failed"), "{sync}");
        assert!(sync.contains("Nobody was refused"), "{sync}");
        let durable = render_send(&json!({ "started": true, "sendingTo": 7 }), "c", true);
        assert!(durable.contains("7 recipient(s) queued"), "{durable}");
    }

    #[test]
    fn render_metrics_handles_empty_and_flat() {
        assert!(render_metrics(&json!({})).contains("confirmed read"));
        let out = render_metrics(&json!({ "sent": 5, "opened": 2, "skipMe": null }));
        assert_eq!(out, "opened: 2\nsent: 5");
    }
}
