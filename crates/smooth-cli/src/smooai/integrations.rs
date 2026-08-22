//! `th api integrations …` — org third-party integrations.
//!
//! Currently just SendGrid (email delivery + inbound), backed by
//! `/organizations/{org_id}/integrations/sendgrid` (get/create/delete)
//! and `…/sendgrid/test`. The API key is never accepted on argv — it
//! comes from `SENDGRID_API_KEY` or an interactive password prompt.

use std::io::IsTerminal;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Password};
use serde_json::json;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// SendGrid email integration (get / create / delete / test).
    Sendgrid {
        #[command(subcommand)]
        cmd: SendgridCmd,
    },
}

#[derive(Subcommand)]
pub enum SendgridCmd {
    /// Show the org's SendGrid integration (API key redacted).
    Get {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create the SendGrid integration. The API key is read from
    /// `SENDGRID_API_KEY` or prompted for — never passed on argv.
    Create {
        /// Verified sender address SendGrid sends from.
        #[arg(long = "from-email")]
        from_email: String,
        /// Address inbound-parse routes replies to.
        #[arg(long = "inbound-email")]
        inbound_email: String,
        /// Optional friendly From name.
        #[arg(long = "from-name")]
        from_name: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete the org's SendGrid integration. Prints the target (org +
    /// host) and confirms before acting; refuses when not attached to a
    /// terminal.
    Delete {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the target and exit without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
    /// Send a test email to verify the configuration.
    Test {
        /// Recipient address for the test email.
        #[arg(long = "to")]
        to: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Sendgrid { cmd } => match cmd {
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
                let api_key = resolve_api_key(std::env::var("SENDGRID_API_KEY").ok())?;
                let body = build_create_body(&from_email, &inbound_email, from_name.as_deref(), &api_key);
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/integrations/sendgrid"), Some(&body))
                        .await
                        .context("POST sendgrid integration")?,
                );
            }
            SendgridCmd::Delete { org, dry_run, yes } => {
                let o = require_active_org(&client, org)?;
                let proceed = crate::destructive::gate(
                    &crate::destructive::Target {
                        verb: "delete",
                        noun: "SendGrid integration",
                        id: "sendgrid",
                        org: &o,
                        severity: crate::destructive::Severity::Standard,
                    },
                    dry_run,
                    yes,
                )?;
                if proceed {
                    print_json(
                        &client
                            .delete(&format!("/organizations/{o}/integrations/sendgrid"))
                            .await
                            .context("DELETE sendgrid integration")?,
                    );
                }
            }
            SendgridCmd::Test { to, org } => {
                let o = require_active_org(&client, org)?;
                let body = build_test_body(&to);
                print_json(
                    &client
                        .post(&format!("/organizations/{o}/integrations/sendgrid/test"), Some(&body))
                        .await
                        .context("POST sendgrid test")?,
                );
            }
        },
    }
    Ok(())
}

/// Resolve the SendGrid API key without ever reading it from argv:
/// use `SENDGRID_API_KEY` when non-empty, otherwise prompt (masked).
/// Errors when the env var is absent/blank and there's no TTY.
fn resolve_api_key(env_value: Option<String>) -> Result<String> {
    if let Some(k) = env_value {
        let k = k.trim();
        if !k.is_empty() {
            return Ok(k.to_string());
        }
    }
    if !std::io::stdin().is_terminal() {
        bail!("SENDGRID_API_KEY is not set and stdin is not a TTY to prompt — export SENDGRID_API_KEY and retry");
    }
    let key = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("SendGrid API key")
        .interact()
        .context("read SendGrid API key")?;
    if key.trim().is_empty() {
        bail!("SendGrid API key must not be empty");
    }
    Ok(key.trim().to_string())
}

fn build_create_body(from_email: &str, inbound_email: &str, from_name: Option<&str>, api_key: &str) -> serde_json::Value {
    let mut body = json!({
        "apiKey": api_key,
        "fromEmail": from_email,
        "inboundEmail": inbound_email,
    });
    if let Some(name) = from_name {
        body["fromName"] = json!(name);
    }
    body
}

fn build_test_body(to: &str) -> serde_json::Value {
    json!({ "to": to })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_has_required_fields_and_omits_absent_from_name() {
        let body = build_create_body("sender@acme.com", "inbound@acme.com", None, "SG.secret");
        assert_eq!(body["apiKey"], "SG.secret");
        assert_eq!(body["fromEmail"], "sender@acme.com");
        assert_eq!(body["inboundEmail"], "inbound@acme.com");
        assert!(body.get("fromName").is_none());
    }

    #[test]
    fn create_body_includes_from_name_when_set() {
        let body = build_create_body("s@acme.com", "in@acme.com", Some("Acme Support"), "SG.k");
        assert_eq!(body["fromName"], "Acme Support");
    }

    #[test]
    fn test_body_carries_recipient() {
        assert_eq!(build_test_body("dev@smoo.ai"), json!({ "to": "dev@smoo.ai" }));
    }

    #[test]
    fn resolve_api_key_prefers_env() {
        assert_eq!(resolve_api_key(Some("SG.fromenv".to_string())).unwrap(), "SG.fromenv");
    }

    #[test]
    fn resolve_api_key_trims_env() {
        assert_eq!(resolve_api_key(Some("  SG.padded \n".to_string())).unwrap(), "SG.padded");
    }

    #[test]
    fn resolve_api_key_errors_without_tty_when_blank() {
        // In `cargo test` stdin is not a TTY, so a blank/absent env value
        // must error rather than block on a prompt.
        assert!(resolve_api_key(None).is_err());
        assert!(resolve_api_key(Some("   ".to_string())).is_err());
    }
}
