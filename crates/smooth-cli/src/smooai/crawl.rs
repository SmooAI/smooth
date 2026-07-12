//! `th crawl …` — scrape a web page through Smoo's in-house crawler (ADR-035).
//!
//! Two tiers, chosen automatically by login state:
//!   - **Logged in** → the authed `POST /organizations/{orgId}/crawl/scrape`
//!     route: full features (JS render, LLM extract), higher limits.
//!   - **Not logged in** → the anonymous free tier `POST /crawl/scrape`
//!     (ADR-005), gated by the bundled publishable client id below. Static-only
//!     (no JS render, no LLM extract) and per-IP quota-capped.
//!
//! Either way it beats the 403s a plain fetch gets. `th auth login` unlocks the
//! full tier — the nudge toward custom org/user signup.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

/// Bundled publishable client id for the `th` public-tools free tier (crawl +
/// web-search). NON-secret by design (see ADR-005): it only asserts "this is the
/// `th` tool" and selects the free tier. Rotated by shipping a new id here and
/// allow-listing both for a window via the `crawlerPublicClientIds` config key;
/// bump the version suffix on rotation. Shared with [`super::websearch`].
pub(crate) const PUBLIC_TOOL_CLIENT_ID: &str = "th-crawl-v1";

#[derive(Subcommand)]
pub enum Cmd {
    /// Scrape a single page → clean markdown (no link-following).
    Scrape {
        /// The page URL to scrape (http/https).
        url: String,
        /// Print the full JSON response (markdown + links/html/… when requested)
        /// instead of just the markdown.
        #[arg(long)]
        json: bool,
        /// Firecrawl-style extra formats to attach, e.g. `--format links`
        /// `--format html`. `markdown` is always returned. Repeatable.
        /// (`summary` needs a login — it's a billed LLM step.)
        #[arg(long = "format", value_name = "FORMAT")]
        formats: Vec<String>,
        /// Override the active org (authed tier only). Falls back to
        /// `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Scrape {
            url,
            json: as_json,
            formats,
            org,
        } => {
            let mut body = json!({ "url": url });
            if !formats.is_empty() {
                body["formats"] = json!(formats);
            }

            // Authed tier when logged in; otherwise the anonymous free tier.
            let resp = match require_authed().await {
                Ok(client) => {
                    let o = require_active_org(&client, org)?;
                    client
                        .post(&format!("/organizations/{o}/crawl/scrape"), Some(&body))
                        .await
                        .context("POST crawl scrape")?
                }
                Err(_) => {
                    eprintln!("• not logged in — using the free tier (static only). Run `th auth login` for JS render + higher limits.");
                    public_scrape(&body).await?
                }
            };

            if as_json {
                print_json(&resp);
            } else {
                // Default surface: just the markdown, so the output pipes cleanly.
                match resp.get("markdown").and_then(|m| m.as_str()) {
                    Some(md) => println!("{md}"),
                    None => print_json(&resp),
                }
            }
        }
    }
    Ok(())
}

/// Anonymous free-tier scrape: an unauthenticated POST to `/crawl/scrape`
/// carrying the bundled publishable client id (no Bearer token).
async fn public_scrape(body: &Value) -> Result<Value> {
    let base = smooth_api_client::base_url();
    let resp = reqwest::Client::new()
        .post(format!("{base}/crawl/scrape"))
        .header("x-crawl-client-id", PUBLIC_TOOL_CLIENT_ID)
        .json(body)
        .send()
        .await
        .context("POST /crawl/scrape")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or(text);
        bail!("crawl failed ({status}): {msg}");
    }
    serde_json::from_str(&text).context("parse crawl response")
}
