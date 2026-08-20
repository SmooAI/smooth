//! `smoo sheets …` — Google Sheets snapshots captured into the org.
//! CLI twin of the hosted MCP `sheets_snapshots` tool (pearl th-a5d991).
//!
//! Snapshots are point-in-time captures, not a live sheet read.

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// List the Google Sheets snapshots captured into this org (spreadsheet,
    /// tab, range, row count, when), newest first — or pass a snapshot id to
    /// read that one with its column headers. Requires a signed-in user
    /// session (not an org API key).
    Snapshots {
        /// A snapshot id from the list — shows that snapshot with its columns.
        snapshot_id: Option<String>,
        /// Page size for the list (1-100, default 25).
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Rows to skip, for paging (default 0).
        #[arg(long, value_name = "N")]
        offset: Option<u64>,
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
        Cmd::Snapshots {
            snapshot_id,
            limit,
            offset,
            json,
            org,
        } => {
            let o = require_active_org(&client, org)?;
            if let Some(id) = snapshot_id {
                let resp = client
                    .get(&format!("/organizations/{o}/sheets/google/snapshots/{}", urlencoding::encode(&id)))
                    .await
                    .context("GET sheet snapshot")?;
                if json {
                    print_json(&resp);
                } else {
                    print_snapshot(&resp);
                }
                return Ok(());
            }
            // 100, not more: the route caps at 100, and a clamp that disagrees
            // with the route's own would report a page size the caller never got.
            let limit = limit.unwrap_or(25).clamp(1, 100);
            let offset = offset.unwrap_or(0);
            let resp = client
                .get(&format!("/organizations/{o}/sheets/google/snapshots?limit={limit}&offset={offset}"))
                .await
                .context("GET sheet snapshots")?;
            if json {
                print_json(&resp);
            } else {
                print_snapshots(&resp, offset);
            }
        }
    }
    Ok(())
}

fn snapshot_line(s: &serde_json::Value) -> String {
    let title = s.get("spreadsheetTitle").and_then(|v| v.as_str()).unwrap_or("(untitled)");
    let sheet = s.get("sheetName").and_then(|v| v.as_str()).unwrap_or("");
    let range = s.get("range").and_then(|v| v.as_str()).unwrap_or("");
    let rows = s.get("rowCount").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let captured = s.get("capturedAt").and_then(|v| v.as_str()).unwrap_or("");
    format!("{title} / {sheet} {range} ({rows} rows) {captured}")
}

fn print_snapshots(body: &serde_json::Value, offset: u64) {
    // Enveloped `{items: […]}` or a bare top-level array — the routes answer
    // with either (same contract as the MCP server's `rows()`).
    let Some(items) = body.as_array().or_else(|| body.get("items").and_then(|v| v.as_array())) else {
        print_json(body);
        return;
    };
    println!();
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), "No sheet snapshots captured yet.".dimmed());
        println!();
        return;
    }
    for s in items {
        let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        println!("  {} {} {}", "○".dimmed(), id.cyan(), snapshot_line(s).bold());
    }
    if let Some(total) = body.get("total").and_then(serde_json::Value::as_u64) {
        if total > offset + items.len() as u64 {
            println!();
            println!(
                "  Showing {}-{} of {total} snapshots (--offset to page).",
                offset + 1,
                offset + items.len() as u64
            );
        }
    }
    println!();
}

fn print_snapshot(body: &serde_json::Value) {
    println!();
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    println!("  {} {}", id.cyan(), snapshot_line(body).bold());
    if let Some(url) = body.get("sourceUrl").and_then(|v| v.as_str()) {
        println!("  {}", url.dimmed());
    }
    if let Some(headers) = body.get("headers").and_then(|h| h.as_array()) {
        let names: Vec<&str> = headers.iter().filter_map(serde_json::Value::as_str).collect();
        println!();
        println!("  Columns: {}", names.join(", "));
    }
    println!();
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::{snapshot_line, Cmd};

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[test]
    fn snapshots_parses_bare() {
        let w = Wrap::try_parse_from(["t", "snapshots"]).expect("bare snapshots must parse");
        assert!(matches!(
            w.cmd,
            Cmd::Snapshots {
                snapshot_id: None,
                limit: None,
                offset: None,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn snapshots_parses_id_positional() {
        let w = Wrap::try_parse_from(["t", "snapshots", "snap-1"]).expect("id must parse");
        match w.cmd {
            Cmd::Snapshots { snapshot_id, .. } => assert_eq!(snapshot_id.as_deref(), Some("snap-1")),
        }
    }

    #[test]
    fn snapshots_parses_limit_offset_json_org() {
        let w = Wrap::try_parse_from(["t", "snapshots", "--limit", "50", "--offset", "25", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::Snapshots { limit, offset, json, org, .. } => {
                assert_eq!(limit, Some(50));
                assert_eq!(offset, Some(25));
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
        }
    }

    #[test]
    fn snapshot_line_renders_fields() {
        let s = json!({
            "spreadsheetTitle": "Leads",
            "sheetName": "Q3",
            "range": "A1:F200",
            "rowCount": 199,
            "capturedAt": "2026-08-01T00:00:00Z"
        });
        assert_eq!(snapshot_line(&s), "Leads / Q3 A1:F200 (199 rows) 2026-08-01T00:00:00Z");
    }
}
