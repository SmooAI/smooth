//! `th crawl …` — scrape a web page through Smoo's in-house crawler (ADR-035).
//!
//! Backed by the authed `POST /organizations/{orgId}/crawl/scrape` route in
//! api-prime, which proxies to the internal `crawler-service` `/scrape`
//! (real browser UA + optional JS render), so it gets pages that a plain
//! unauthenticated fetch 403s on. Available to any authenticated member of an
//! org — signing up (custom org/user) is what unlocks it.

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::json;

use super::{print_json, require_active_org, require_authed};

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
        #[arg(long = "format", value_name = "FORMAT")]
        formats: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the
        /// credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Scrape {
            url,
            json: as_json,
            formats,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let mut body = json!({ "url": url });
            if !formats.is_empty() {
                body["formats"] = json!(formats);
            }
            let resp = client
                .post(&format!("/organizations/{o}/crawl/scrape"), Some(&body))
                .await
                .context("POST crawl scrape")?;
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
