//! `th smoo drive …` — the CLI twin of the hosted MCP's `drive_search` /
//! `drive_read` tools (SMOODEV-3545).
//!
//! Both run live against YOUR Google Drive, as your own Google account, on the
//! `drive_restricted` grant (Integrations → Drive · Restricted), so Google
//! enforces Drive permissions and only files you can already open come back.
//! The routes are user-only (`/organizations/{org}/drive/google/…`): an org API
//! key has no Drive, so this needs `smoo auth login`.
//!
//! A 409 from either route is an ANSWER, not a fault: you have no live Drive
//! connection of your own (never connected, revoked, or expired). The error
//! says so and names the command that fixes it.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::Value;

use super::{print_json, require_active_org, require_user_session};

#[derive(Subcommand)]
pub enum Cmd {
    /// Full-text search of your Google Drive — file names AND contents, across
    /// My Drive, shared-with-me and shared drives.
    Search {
        /// Words to look for.
        query: String,
        /// Max files, 1–50 (default 20).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: Option<u32>,
        /// `nextPageToken` from a previous search, for the next page.
        #[arg(long = "page-token")]
        page_token: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Print one Drive file's text (Docs → markdown, Sheets → CSV of the first
    /// sheet, Slides → text). PDFs and Office files are not readable inline yet.
    Read {
        /// Google Drive file id (from `drive search`).
        file_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON (content + metadata) instead of just the text.
        #[arg(long)]
        json: bool,
    },
}

/// A Drive file id bound for a URL path: Smoo-id alphabet, up to 128 chars
/// (Drive ids are longer than Smoo ids). Mirrors the MCP's `drive_file_id`.
fn drive_file_id(id: &str) -> Result<String> {
    let id = id.trim();
    let ok = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("file id must be a Google Drive file id from `smoo drive search`; got {id:?}");
    }
    Ok(id.to_string())
}

/// `GET drive/google/search?q=…&limit=N[&pageToken=…]` (limit default 20, ≤50).
fn search_path(org: &str, query: &str, limit: Option<u32>, page_token: Option<&str>) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        bail!("the query is empty — pass the words to search Drive for");
    }
    let mut p = format!(
        "/organizations/{org}/drive/google/search?q={}&limit={}",
        urlencoding::encode(q),
        limit.unwrap_or(20).clamp(1, 50)
    );
    if let Some(t) = page_token.map(str::trim).filter(|t| !t.is_empty()) {
        p.push_str(&format!("&pageToken={}", urlencoding::encode(t)));
    }
    Ok(p)
}

fn read_path(org: &str, file_id: &str) -> Result<String> {
    Ok(format!("/organizations/{org}/drive/google/files/{}/content", drive_file_id(file_id)?))
}

/// Turn a 409 into "connect Drive" with the fix; anything else keeps its context.
fn drive_error(err: anyhow::Error, what: &str) -> anyhow::Error {
    if format!("{err:#}").contains("HTTP 409") {
        anyhow::anyhow!(
            "Google Drive is not connected for you, so nothing was read. Connect it with \
             `smoo integrations connect google --purpose drive_restricted`, then retry. ({err:#})"
        )
    } else {
        err.context(what.to_string())
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn render_search(query: &str, body: &Value) {
    let files = body.get("files").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let account = body.get("account").and_then(Value::as_str).map(|a| format!(" (as {a})")).unwrap_or_default();
    println!();
    if files.is_empty() {
        println!("  {} no Drive files match {query:?}{account}", "●".dimmed());
    }
    for f in files {
        println!(
            "  {} {} {}",
            "○".dimmed(),
            s(f, "name").bold(),
            format!("{}  {}  {}", s(f, "mimeType"), s(f, "owner"), s(f, "modifiedTime")).dimmed()
        );
        println!("      {}  {}", s(f, "id").cyan(), s(f, "webViewLink").dimmed());
    }
    println!();
    if body.get("incompleteSearch").and_then(Value::as_bool).unwrap_or(false) {
        anstream::eprintln!("  {} Google reported this search as INCOMPLETE — try a more specific query", "!".yellow());
    }
    if let Some(t) = body.get("nextPageToken").and_then(Value::as_str) {
        anstream::eprintln!("  {} more results: --page-token {t}", "●".dimmed());
    }
}

/// The file's text on stdout (pipe-friendly); notes about truncation or why
/// nothing was read go to stderr.
fn render_read(body: &Value) -> Result<()> {
    let name = if s(body, "name").is_empty() { "this file" } else { s(body, "name") };
    let content = s(body, "content");
    if content.trim().is_empty() {
        let reason = if s(body, "reason").is_empty() {
            "it has no readable text"
        } else {
            s(body, "reason")
        };
        let link = s(body, "webViewLink");
        bail!(
            "{name:?} was found but not read: {reason}{}",
            if link.is_empty() {
                String::new()
            } else {
                format!(" — open it in Drive: {link}")
            }
        );
    }
    if s(body, "source") == "export:text/csv" {
        anstream::eprintln!("  {} first sheet only, as CSV", "●".dimmed());
    }
    let chars = content.chars().count();
    if let Some(total) = body.get("totalCharacters").and_then(Value::as_u64) {
        if total as usize > chars {
            anstream::eprintln!("  {} TRUNCATED — showing the first {chars} of {total} characters", "!".yellow());
        }
    }
    println!("{content}");
    Ok(())
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    // Validate before asking for a session, so a typo fails without a login.
    let client_needed = || async {
        require_user_session()
            .await
            .context("Google Drive is read as YOU — run `smoo auth login` (an org API key has no Drive)")
    };
    match cmd {
        Cmd::Search {
            query,
            limit,
            page_token,
            org,
            json,
        } => {
            search_path("_", &query, limit, page_token.as_deref())?;
            let client = client_needed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&search_path(&o, &query, limit, page_token.as_deref())?)
                .await
                .map_err(|e| drive_error(e, "GET drive/google/search"))?;
            if json {
                print_json(&body);
            } else {
                render_search(query.trim(), &body);
            }
        }
        Cmd::Read { file_id, org, json } => {
            drive_file_id(&file_id)?;
            let client = client_needed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&read_path(&o, &file_id)?)
                .await
                .map_err(|e| drive_error(e, "GET drive/google/files/{id}/content"))?;
            if json {
                print_json(&body);
            } else {
                render_read(&body)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut v = vec!["t"];
        v.extend_from_slice(args);
        Wrap::try_parse_from(v).map(|w| w.cmd)
    }

    #[test]
    fn verbs_parse() {
        assert!(matches!(
            parse(&["search", "onboarding doc", "--limit", "5", "--page-token", "tok", "--json"]).unwrap(),
            Cmd::Search {
                limit: Some(5),
                json: true,
                ..
            }
        ));
        assert!(matches!(parse(&["read", "1AbC_-x"]).unwrap(), Cmd::Read { json: false, .. }));
        assert!(parse(&["search"]).is_err(), "query is required");
        assert!(parse(&["read"]).is_err(), "file id is required");
    }

    #[test]
    fn search_limit_is_bounded() {
        assert!(parse(&["search", "q", "--limit", "0"]).is_err());
        assert!(parse(&["search", "q", "--limit", "51"]).is_err());
        assert!(parse(&["search", "q", "--limit", "50"]).is_ok());
    }

    #[test]
    fn search_path_matches_the_mcp_route() {
        assert_eq!(
            search_path("o1", " pricing & plans ", None, None).unwrap(),
            "/organizations/o1/drive/google/search?q=pricing%20%26%20plans&limit=20"
        );
        assert_eq!(
            search_path("o1", "x", Some(50), Some("a/b=")).unwrap(),
            "/organizations/o1/drive/google/search?q=x&limit=50&pageToken=a%2Fb%3D"
        );
        assert_eq!(
            search_path("o1", "x", None, Some("  ")).unwrap(),
            "/organizations/o1/drive/google/search?q=x&limit=20"
        );
        assert!(search_path("o1", "   ", None, None).is_err(), "blank query refused");
    }

    #[test]
    fn read_path_validates_the_file_id() {
        let long = "a".repeat(128);
        assert_eq!(
            read_path("o1", "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms").unwrap(),
            "/organizations/o1/drive/google/files/1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms/content"
        );
        assert!(read_path("o1", &long).is_ok(), "drive ids may exceed the 64-char Smoo-id cap");
        for bad in ["", "a/b", "a?b", "a b", "..", &"a".repeat(129)] {
            assert!(read_path("o1", bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_409_becomes_the_connect_hint() {
        let e = drive_error(anyhow::anyhow!("HTTP 409 Conflict: no drive grant"), "GET x");
        assert!(format!("{e:#}").contains("smoo integrations connect google --purpose drive_restricted"));
        let e = drive_error(anyhow::anyhow!("HTTP 500 boom"), "GET x");
        let msg = format!("{e:#}");
        assert!(msg.starts_with("GET x") && msg.contains("HTTP 500"), "other errors keep their context: {msg}");
    }

    #[test]
    fn unreadable_file_is_an_error_naming_the_reason() {
        let err = render_read(&json!({ "name": "deck.pdf", "reason": "PDFs are not readable inline yet", "webViewLink": "https://d/x" }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("PDFs are not readable inline yet") && err.contains("https://d/x"), "{err}");
        assert!(render_read(&json!({ "name": "a", "content": "hello" })).is_ok());
    }
}
