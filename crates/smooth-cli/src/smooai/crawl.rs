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
        /// LLM structured extraction (authed tier only). A JSON schema/spec is
        /// sent verbatim; any other string is wrapped as `{"prompt": <SPEC>}`.
        #[arg(long, value_name = "SPEC")]
        extract: Option<String>,
        /// Also capture a screenshot (adds `screenshot` to the formats).
        #[arg(long)]
        screenshot: bool,
        /// JS-render mode, e.g. `auto` or `always` (authed tier only).
        #[arg(long, value_name = "MODE")]
        render: Option<String>,
        /// Override the active org (authed tier only). Falls back to
        /// `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Crawl a whole site from a seed URL → markdown for every page (authed).
    Crawl {
        /// The seed URL to crawl from.
        url: String,
        /// Cap the number of pages crawled.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Max link-following depth from the seed (sent as `maxDiscoveryDepth`).
        #[arg(long = "max-depth", value_name = "N")]
        max_depth: Option<u64>,
        /// LLM structured extraction per page: a JSON spec is sent verbatim, any
        /// other string is wrapped as `{"prompt": <SPEC>}`.
        #[arg(long, value_name = "SPEC")]
        extract: Option<String>,
        /// Print the full JSON response instead of the compact summary.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the
        /// credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Map a site → the list of discoverable URLs (no content; authed).
    Map {
        /// The URL to map.
        url: String,
        /// Only return links matching this search term.
        #[arg(long, value_name = "TERM")]
        search: Option<String>,
        /// Cap the number of links returned.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Include links on subdomains of the target.
        #[arg(long = "include-subdomains")]
        include_subdomains: bool,
        /// Print the full JSON response instead of one link per line.
        #[arg(long)]
        json: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the
        /// credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// Parse an `--extract` spec: valid JSON is used verbatim, anything else is
/// wrapped as `{"prompt": <SPEC>}`.
fn parse_extract(spec: &str) -> Value {
    serde_json::from_str::<Value>(spec).unwrap_or_else(|_| json!({ "prompt": spec }))
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Scrape {
            url,
            json: as_json,
            mut formats,
            extract,
            screenshot,
            render,
            org,
        } => {
            if screenshot {
                formats.push("screenshot".to_string());
            }
            let mut body = json!({ "url": url });
            if !formats.is_empty() {
                body["formats"] = json!(formats);
            }
            if let Some(spec) = &extract {
                body["extract"] = parse_extract(spec);
            }
            if let Some(mode) = &render {
                body["render"] = json!(mode);
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
                    if extract.is_some() || render.is_some() {
                        eprintln!("• note: --extract/--render are authed-only; the free tier will reject them.");
                    }
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
        Cmd::Crawl {
            url,
            limit,
            max_depth,
            extract,
            json: as_json,
            org,
        } => {
            let client = require_authed().await.context("`th crawl crawl` requires login — run `th auth login`")?;
            let o = require_active_org(&client, org)?;
            let mut body = json!({ "url": url });
            if let Some(n) = limit {
                body["limit"] = json!(n);
            }
            if let Some(n) = max_depth {
                body["maxDiscoveryDepth"] = json!(n);
            }
            if let Some(spec) = &extract {
                body["extract"] = parse_extract(spec);
            }
            let resp = client
                .post(&format!("/organizations/{o}/crawl/crawl"), Some(&body))
                .await
                .context("POST crawl crawl")?;
            if as_json {
                print_json(&resp);
            } else {
                let completed = resp.get("completed").and_then(Value::as_u64).unwrap_or(0);
                let total = resp.get("total").and_then(Value::as_u64).unwrap_or(0);
                println!("{completed}/{total} pages crawled");
                if let Some(data) = resp.get("data").and_then(|d| d.as_array()) {
                    for page in data {
                        if let Some(u) = page.get("url").and_then(|v| v.as_str()) {
                            println!("{u}");
                        }
                    }
                }
            }
        }
        Cmd::Map {
            url,
            search,
            limit,
            include_subdomains,
            json: as_json,
            org,
        } => {
            let client = require_authed().await.context("`th crawl map` requires login — run `th auth login`")?;
            let o = require_active_org(&client, org)?;
            let mut body = json!({ "url": url });
            if let Some(s) = &search {
                body["search"] = json!(s);
            }
            if let Some(n) = limit {
                body["limit"] = json!(n);
            }
            if include_subdomains {
                body["includeSubdomains"] = json!(true);
            }
            let resp = client
                .post(&format!("/organizations/{o}/crawl/map"), Some(&body))
                .await
                .context("POST crawl map")?;
            if as_json {
                print_json(&resp);
            } else {
                match resp.get("links").and_then(|l| l.as_array()) {
                    Some(links) => {
                        for link in links {
                            match link.as_str() {
                                Some(s) => println!("{s}"),
                                None => println!("{link}"),
                            }
                        }
                    }
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
