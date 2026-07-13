//! `th search …` — agentic web search over Smoo's in-house search service
//! (SMOODEV-2573 / ADR-088: self-hosted SearXNG index + our crawler + LLM answer
//! synthesis — Tavily was retired in SMOODEV-2592), a companion to `th crawl`.
//! Same two-tier model as crawl:
//!
//!   - **Logged in** → authed `POST /organizations/{orgId}/search`: full options
//!     (advanced depth, higher `--max`, `--answer`).
//!   - **Not logged in** → anonymous free tier `POST /search` (ADR-005), gated by
//!     the bundled `th` public-tools client id, basic depth + capped results.
//!
//! `th auth login` unlocks the full tier.
//!
//! The top-level command is `th search <query>`. `th web-search search <query>`
//! is kept as a hidden back-compat alias for the form shipped in v0.18.0.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::crawl::PUBLIC_TOOL_CLIENT_ID;
use super::{print_json, require_active_org, require_authed};

/// Shared args for the top-level `th search` and the legacy `th web-search search`.
#[derive(Args)]
pub struct SearchArgs {
    /// The search query.
    pub query: String,
    /// Print the full JSON response (results + optional answer) instead of
    /// the compact list.
    #[arg(long)]
    pub json: bool,
    /// Max results to return (authed tier; the free tier caps lower).
    #[arg(long, value_name = "N")]
    pub max: Option<u64>,
    /// Search depth: `basic` (default) or `advanced` (authed tier only).
    #[arg(long)]
    pub depth: Option<String>,
    /// Include a synthesized answer (authed tier only — a billed LLM step).
    #[arg(long)]
    pub answer: bool,
    /// Override the active org (authed tier). Falls back to `SMOOAI_ORG_ID`
    /// then the credentials file's `active_org_id`.
    #[arg(long = "org-id", visible_alias = "org")]
    pub org: Option<String>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Search the web → ranked results (title, url, snippet).
    Search {
        #[command(flatten)]
        args: SearchArgs,
    },
}

/// Legacy `th web-search <cmd>` dispatch (hidden). Delegates to [`run`].
pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Search { args } => run(args).await,
    }
}

/// The actual search, shared by `th search` and `th web-search search`.
pub async fn run(args: SearchArgs) -> Result<()> {
    let SearchArgs {
        query,
        json: as_json,
        max,
        depth,
        answer,
        org,
    } = args;

    let mut body = json!({ "query": query });
    if let Some(m) = max {
        body["maxResults"] = json!(m);
    }
    if let Some(d) = depth {
        body["searchDepth"] = json!(d);
    }
    if answer {
        body["includeAnswer"] = json!(true);
    }

    let resp = match require_authed().await {
        Ok(client) => {
            let o = require_active_org(&client, org)?;
            client.post(&format!("/organizations/{o}/search"), Some(&body)).await.context("POST search")?
        }
        Err(_) => {
            eprintln!("• not logged in — using the free tier (basic depth, capped results). Run `th auth login` for advanced search + answers.");
            public_search(&body).await?
        }
    };

    if as_json {
        print_json(&resp);
    } else {
        print_results(&resp);
    }
    Ok(())
}

/// Anonymous free-tier search: unauthenticated POST to `/search` carrying the
/// bundled `th` public-tools client id (no Bearer token).
async fn public_search(body: &Value) -> Result<Value> {
    let base = smooth_api_client::base_url();
    let resp = reqwest::Client::new()
        .post(format!("{base}/search"))
        .header("x-th-client-id", PUBLIC_TOOL_CLIENT_ID)
        .json(body)
        .send()
        .await
        .context("POST /search")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or(text);
        bail!("web search failed ({status}): {msg}");
    }
    serde_json::from_str(&text).context("parse search response")
}

/// Compact human-readable rendering: an optional answer, then a numbered list of
/// `title — url` with a one-line snippet each. Falls back to JSON on an odd shape.
fn print_results(resp: &Value) {
    if let Some(answer) = resp.get("answer").and_then(|a| a.as_str()) {
        if !answer.trim().is_empty() {
            println!("{answer}\n");
        }
    }
    match resp.get("results").and_then(|r| r.as_array()) {
        Some(results) if !results.is_empty() => {
            for (i, r) in results.iter().enumerate() {
                let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
                let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
                println!("{}. {title}\n   {url}", i + 1);
                if let Some(c) = r.get("content").and_then(|v| v.as_str()) {
                    let snippet: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
                    let snippet = snippet.chars().take(200).collect::<String>();
                    if !snippet.is_empty() {
                        println!("   {snippet}");
                    }
                }
            }
        }
        _ => print_json(resp),
    }
}
