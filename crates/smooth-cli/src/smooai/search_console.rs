//! `smoo search-console …` — Google Search Console sites and top queries.
//! CLI twin of the hosted MCP `search_console_queries` tool (pearl th-a5d991).

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Top search queries (clicks, impressions, CTR, average position) for a
    /// verified Search Console property. Omit the site to list the verified
    /// sites first — pass `siteUrl` exactly as it reads there (e.g.
    /// `sc-domain:smoo.ai`). Requires a signed-in user session (not an org
    /// API key).
    Queries {
        /// A verified property's `siteUrl`; omit to list the verified sites.
        site_url: Option<String>,
        /// Trailing window in days (1-90, default 28).
        #[arg(long, value_name = "N")]
        days: Option<u64>,
        /// Max query rows (1-500, default 25).
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Print raw JSON instead of the compact listing.
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
        Cmd::Queries {
            site_url,
            days,
            limit,
            json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            let Some(site_url) = site_url.filter(|v| !v.trim().is_empty()) else {
                let resp = client
                    .get(&format!("/organizations/{o}/websites/google/search-console/sites"))
                    .await
                    .context("GET Search Console sites")?;
                if json {
                    print_json(&resp);
                } else {
                    print_sites(&resp);
                }
                return Ok(());
            };
            // Clamped here rather than relying on the route's own clamp, so the
            // window in the answer is the window that was actually served.
            let days = days.unwrap_or(28).clamp(1, 90);
            let limit = limit.unwrap_or(25).clamp(1, 500);
            let resp = client
                .get(&format!(
                    "/organizations/{o}/websites/google/search-console/queries?siteUrl={}&days={days}&limit={limit}",
                    urlencoding::encode(&site_url)
                ))
                .await
                .context("GET Search Console queries")?;
            if json {
                print_json(&resp);
            } else {
                print_queries(&resp, days);
            }
        }
    }
    Ok(())
}

/// Rows under `key`, accepting the enveloped shape or a bare top-level array —
/// the routes answer with either (same contract as the MCP server's `rows()`).
fn rows<'a>(body: &'a serde_json::Value, key: &str) -> Option<&'a Vec<serde_json::Value>> {
    body.as_array().or_else(|| body.get(key).and_then(|v| v.as_array()))
}

fn print_sites(body: &serde_json::Value) {
    let Some(rows) = rows(body, "sites") else {
        print_json(body);
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No verified Search Console sites yet.".dimmed());
        println!();
        return;
    }
    for s in rows {
        let url = s.get("siteUrl").and_then(|v| v.as_str()).unwrap_or("?");
        let perm = s.get("permissionLevel").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {}", "○".dimmed(), url.cyan(), format!("[{perm}]").dimmed());
    }
    println!();
    println!("  Next: smoo search-console queries <siteUrl>");
    println!();
}

fn print_queries(body: &serde_json::Value, days: u64) {
    let Some(rows) = rows(body, "rows") else {
        print_json(body);
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), format!("No query data in the last {days} days.").dimmed());
        println!();
        return;
    }
    println!(
        "  {}",
        format!("top queries, last {days} days (clicks / impressions / ctr / position)").dimmed()
    );
    println!();
    for r in rows {
        let query = r.get("query").and_then(|v| v.as_str()).unwrap_or("?");
        let clicks = r.get("clicks").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let impressions = r.get("impressions").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let ctr = r.get("ctr").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let position = r.get("position").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        println!(
            "  {} {} {}",
            "○".dimmed(),
            query.bold(),
            format!("{clicks} / {impressions} / {:.1}% / {position:.1}", ctr * 100.0).dimmed()
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cmd;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn queries_parses_bare() {
        let w = Wrap::try_parse_from(["t", "queries"]).expect("bare queries must parse");
        assert!(matches!(
            w.cmd,
            Cmd::Queries {
                site_url: None,
                days: None,
                limit: None,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn queries_parses_site_url_positional() {
        let w = Wrap::try_parse_from(["t", "queries", "sc-domain:smoo.ai"]).expect("site url must parse");
        match w.cmd {
            Cmd::Queries { site_url, .. } => assert_eq!(site_url.as_deref(), Some("sc-domain:smoo.ai")),
        }
    }

    #[test]
    fn queries_parses_days_limit_json_org() {
        let w =
            Wrap::try_parse_from(["t", "queries", "sc-domain:smoo.ai", "--days", "7", "--limit", "100", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::Queries { days, limit, json, org, .. } => {
                assert_eq!(days, Some(7));
                assert_eq!(limit, Some(100));
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
        }
    }
}
