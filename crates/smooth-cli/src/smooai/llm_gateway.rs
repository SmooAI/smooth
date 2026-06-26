//! `th llm …` — org LLM gateway keys (the `api.smoo.ai`
//! `/organizations/{org_id}/llm-gateway/*` surface).
//!
//! Mint and rotate an org's persistent `llm.smoo.ai` key, manage
//! additional named keys, and inspect spend. These routes authenticate
//! as the **user** (Supabase JWT) and are org-admin-gated — they 401
//! under an M2M token, so this surface uses [`UserClient`], not the
//! M2M-capable `SmoothApiClient`. A master/super admin can target a
//! child org with `--org-id <child>` (the user JWT acts cross-org).
//!
//! Keys are LiteLLM virtual keys scoped to the org's team/budget; the
//! key VALUE is returned exactly once at mint/rotate time.

use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the org's LLM gateway status — the masked key (with
    /// create/rotate timestamps) and month-to-date spend. Run this
    /// before `create-key` to see whether the org already has one.
    Overview {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then
        /// the credentials file's `active_org_id`. A master admin can
        /// target a child org here.
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty summary.
        #[arg(long)]
        json: bool,
    },
    /// Show LLM spend broken down by model and by day over a window
    /// (default 30 days, clamped to 1–90).
    Usage {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Window length in days (clamped to 1–90).
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Emit raw JSON (default — the breakdown is a timeseries).
        #[arg(long)]
        json: bool,
    },
    /// Mint the org's persistent ("default") LLM gateway key. Provisions
    /// a LiteLLM team + virtual key scoped to the org's budget and
    /// prints the key VALUE exactly once — store it immediately. Fails
    /// with 409 if the org already has a key (use `rotate-key`).
    CreateKey {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Rotate the org's persistent key — the old key is invalidated and
    /// a new value is printed once. Use after a suspected leak.
    RotateKey {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Manage additional named keys beyond the default — e.g. one per
    /// service or environment.
    Keys {
        #[command(subcommand)]
        cmd: KeysCmd,
    },
}

#[derive(Subcommand)]
pub enum KeysCmd {
    /// List the org's named keys (masked).
    List {
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty list.
        #[arg(long)]
        json: bool,
    },
    /// Create a new named key — prints the value exactly once.
    Create {
        /// Key name, unique per org (lowercase letters, digits, dashes).
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Rotate a named key — invalidates the old value, prints the new one once.
    Rotate {
        /// Name of the key to rotate.
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Revoke (soft-delete) a named key. It stops working at the
    /// provider immediately; the name can be re-minted later.
    Delete {
        /// Name of the key to revoke.
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session()?;
    match cmd {
        Cmd::Overview { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/overview"))
                .await
                .context("GET llm-gateway overview")?;
            if json {
                print_json(&resp);
            } else {
                print_overview(&resp);
            }
        }
        Cmd::Usage { org_id, days, json: _ } => {
            let org = crate::active_org::resolve(org_id)?;
            let days = days.clamp(1, 90);
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/usage?days={days}"))
                .await
                .context("GET llm-gateway usage")?;
            // The usage payload is a per-model + per-day timeseries; JSON is
            // the useful form, so it always prints as JSON.
            print_json(&resp);
        }
        Cmd::CreateKey { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/create-key"), &json!({}))
                .await
                .context("POST llm-gateway create-key")?;
            print_minted_key(&resp, json);
        }
        Cmd::RotateKey { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/rotate-key"), &json!({}))
                .await
                .context("POST llm-gateway rotate-key")?;
            print_minted_key(&resp, json);
        }
        Cmd::Keys { cmd } => keys(cmd, &client).await?,
    }
    Ok(())
}

async fn keys(cmd: KeysCmd, client: &UserClient) -> Result<()> {
    match cmd {
        KeysCmd::List { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/keys"))
                .await
                .context("GET llm-gateway keys")?;
            if json {
                print_json(&resp);
            } else {
                print_keys(&resp);
            }
        }
        KeysCmd::Create { name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/keys"), &json!({ "name": name }))
                .await
                .context("POST llm-gateway named key")?;
            print_minted_key(&resp, json);
        }
        KeysCmd::Rotate { name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/keys/{name}/rotate"), &json!({}))
                .await
                .context("POST llm-gateway named key rotate")?;
            print_minted_key(&resp, json);
        }
        KeysCmd::Delete { name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .delete(&format!("/organizations/{org}/llm-gateway/keys/{name}"))
                .await
                .context("DELETE llm-gateway named key")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} revoked key {}", "✓".green(), name.bold());
                println!();
            }
        }
    }
    Ok(())
}

/// Print a freshly-minted key result. The key VALUE is returned by the
/// API exactly once, so make it impossible to miss and remind the user
/// to store it now.
fn print_minted_key(resp: &Value, json: bool) {
    if json {
        print_json(resp);
        return;
    }
    let Some(key) = resp.get("key").and_then(Value::as_str) else {
        // Shape we didn't expect — fall back to raw JSON rather than
        // swallow the response.
        print_json(resp);
        return;
    };
    let mask = resp.get("mask").and_then(Value::as_str).unwrap_or("");
    println!();
    println!("  {} LLM gateway key — shown once, store it now:", "✓".green().bold());
    println!();
    println!("    {}", key.bold());
    if !mask.is_empty() {
        println!("    {} {}", "mask".dimmed(), mask.dimmed());
    }
    println!();
    println!(
        "  {} wire it into the gateway provider: {}",
        "→".dimmed(),
        "th model login smooai-gateway".bold()
    );
    println!();
}

/// Pretty-print the `overview` payload (`{ key, spendMtd }`).
fn print_overview(resp: &Value) {
    println!();
    match resp.get("key") {
        Some(Value::Object(k)) => {
            let mask = k.get("mask").and_then(Value::as_str).unwrap_or("?");
            println!("  {} {}", "key".dimmed(), mask);
            if let Some(c) = k.get("createdAt").and_then(Value::as_str) {
                println!("  {} {}", "created".dimmed(), c);
            }
            if let Some(r) = k.get("rotatedAt").and_then(Value::as_str) {
                println!("  {} {}", "rotated".dimmed(), r);
            }
        }
        _ => {
            println!("  {} no key yet — run {} to mint one", "●".dimmed(), "th llm create-key".bold());
        }
    }
    if let Some(s) = resp.get("spendMtd") {
        let spend = s.get("totalSpendUsd").and_then(Value::as_f64).unwrap_or(0.0);
        let tokens = s.get("totalTokens").and_then(Value::as_u64).unwrap_or(0);
        let reqs = s.get("requestCount").and_then(Value::as_u64).unwrap_or(0);
        println!("  {} ${spend:.2} · {tokens} tokens · {reqs} requests (MTD)", "spend".dimmed());
    }
    println!();
}

/// Pretty-print the `keys` list payload (`{ keys: [...] }`).
fn print_keys(resp: &Value) {
    println!();
    match resp.get("keys").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => {
            for k in arr {
                let name = k.get("name").and_then(Value::as_str).unwrap_or("?");
                let mask = k.get("mask").and_then(Value::as_str).unwrap_or("");
                println!("  {} {}  {}", "○".dimmed(), name.bold(), mask.dimmed());
            }
        }
        _ => println!("  {} {}", "●".dimmed(), "no named keys".dimmed()),
    }
    println!();
}
