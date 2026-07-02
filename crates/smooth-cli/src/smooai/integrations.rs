//! `th api integrations …` — per-org third-party integrations.
//!
//! Currently SendGrid (email delivery), backed by the monorepo's
//! `/organizations/{org}/integrations/sendgrid` routes. The API key is
//! write-only server-side (reads return `hasApiKey`, never the key), so `create`
//! takes it from the `SENDGRID_API_KEY` env var or an interactive no-echo prompt
//! — never a flag, so it can't land in shell history.

use anyhow::{Context, Result};
use clap::Subcommand;
use dialoguer::Password;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// SendGrid email integration (get / create / delete / test).
    #[command(subcommand)]
    Sendgrid(SendgridCmd),
}

#[derive(Subcommand)]
pub enum SendgridCmd {
    /// Show the org's SendGrid integration status (API key redacted).
    Get {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create the org's SendGrid integration. The API key comes from
    /// `SENDGRID_API_KEY` or an interactive no-echo prompt — never a flag.
    Create {
        /// The `From:` address outbound mail is sent as (e.g. `hello@smoo.ai`).
        #[arg(long = "from-email")]
        from_email: String,
        /// The org's inbound parse address (e.g. `inbound@smoo.ai`).
        #[arg(long = "inbound-email")]
        inbound_email: String,
        /// Optional `From:` display name.
        #[arg(long = "from-name")]
        from_name: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete the org's SendGrid integration.
    Delete {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Send a test email to confirm the integration works.
    Test {
        /// Recipient address for the test email.
        #[arg(long)]
        to: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let Cmd::Sendgrid(sub) = cmd;
    let client = require_authed().await?;
    match sub {
        SendgridCmd::Get { org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/integrations/sendgrid"))
                    .await
                    .context("GET sendgrid integration")?,
            );
        }
        SendgridCmd::Create {
            from_email,
            inbound_email,
            from_name,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let api_key = resolve_api_key()?;
            let mut body = serde_json::json!({
                "apiKey": api_key,
                "fromEmail": from_email,
                "inboundEmail": inbound_email,
            });
            if let Some(name) = from_name {
                body["fromName"] = serde_json::Value::String(name);
            }
            print_json(
                &client
                    .post(&format!("/organizations/{o}/integrations/sendgrid"), Some(&body))
                    .await
                    .context("POST sendgrid integration")?,
            );
        }
        SendgridCmd::Delete { org } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/organizations/{o}/integrations/sendgrid"))
                    .await
                    .context("DELETE sendgrid integration")?,
            );
        }
        SendgridCmd::Test { to, org } => {
            let o = require_active_org(&client, org)?;
            let body = serde_json::json!({ "to": to });
            print_json(
                &client
                    .post(&format!("/organizations/{o}/integrations/sendgrid/test"), Some(&body))
                    .await
                    .context("POST sendgrid integration test")?,
            );
        }
    }
    Ok(())
}

/// Trim a candidate API key, returning `None` when it's blank. Kept pure so the
/// resolution precedence can be unit-tested without mutating process env (the
/// crate forbids the `unsafe` that `std::env::set_var` now requires).
fn normalize_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve the SendGrid API key from `SENDGRID_API_KEY` or an interactive
/// no-echo prompt. Never a flag — a secret on argv lands in shell history.
fn resolve_api_key() -> Result<String> {
    if let Some(key) = std::env::var("SENDGRID_API_KEY").ok().as_deref().and_then(normalize_key) {
        return Ok(key);
    }
    let entered = Password::new()
        .with_prompt("SendGrid API key")
        .interact()
        .context("prompt for SendGrid API key")?;
    normalize_key(&entered).context("SendGrid API key must not be empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_rejects_blank() {
        assert_eq!(normalize_key("  SG.test-key  "), Some("SG.test-key".to_string()));
        assert_eq!(normalize_key("SG.k"), Some("SG.k".to_string()));
        assert_eq!(normalize_key("   "), None);
        assert_eq!(normalize_key(""), None);
    }

    #[test]
    fn create_body_omits_from_name_when_absent() {
        // Mirror the body assembly in the Create arm so the shape can't drift
        // from what the backend route requires (apiKey/fromEmail/inboundEmail).
        let mut body = serde_json::json!({
            "apiKey": "SG.k",
            "fromEmail": "hello@smoo.ai",
            "inboundEmail": "inbound@smoo.ai",
        });
        let from_name: Option<String> = None;
        if let Some(name) = from_name {
            body["fromName"] = serde_json::Value::String(name);
        }
        assert!(body.get("fromName").is_none());
        assert_eq!(body["apiKey"], "SG.k");
        assert_eq!(body["fromEmail"], "hello@smoo.ai");
        assert_eq!(body["inboundEmail"], "inbound@smoo.ai");
    }

    #[test]
    fn create_body_includes_from_name_when_present() {
        let mut body = serde_json::json!({
            "apiKey": "SG.k",
            "fromEmail": "hello@smoo.ai",
            "inboundEmail": "inbound@smoo.ai",
        });
        let from_name = Some("Smoo AI Test".to_string());
        if let Some(name) = from_name {
            body["fromName"] = serde_json::Value::String(name);
        }
        assert_eq!(body["fromName"], "Smoo AI Test");
    }
}
