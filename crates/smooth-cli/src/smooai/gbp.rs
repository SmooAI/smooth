//! `smoo gbp …` — Google Business Profile reviews.
//! CLI twin of the hosted MCP `gbp_reviews` tool (pearl th-a5d991).
//!
//! A discovery ladder, same as the MCP tool: no `--account` lists the
//! connected accounts, `--account` alone lists its locations, and
//! `--account` + `--location` reads the reviews.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Read the customer reviews on this org's Google Business Profile.
    /// Run with no flags to list the connected GBP accounts, then with
    /// `--account` to list that account's locations, then with both
    /// `--account` and `--location` for the reviews. Pass the `name`
    /// (resource name) field from each step, not the display title.
    /// Requires a signed-in user session (not an org API key).
    Reviews {
        /// GBP account resource name (`accounts/123…`), from the account list.
        #[arg(long, value_name = "ACCOUNTS/ID")]
        account: Option<String>,
        /// Location resource name (`locations/456…`), from the location list.
        #[arg(long, value_name = "LOCATIONS/ID")]
        location: Option<String>,
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
        Cmd::Reviews { account, location, json, org } => {
            let o = require_active_org(&client, org)?;
            let account = account.filter(|v| !v.trim().is_empty());
            let location = location.filter(|v| !v.trim().is_empty());
            let Some(account) = account else {
                if location.is_some() {
                    bail!("--location needs --account too — run `smoo gbp reviews` with no flags to list the accounts first");
                }
                let resp = client
                    .get(&format!("/organizations/{o}/business/google/accounts"))
                    .await
                    .context("GET GBP accounts")?;
                if json {
                    print_json(&resp);
                } else {
                    print_accounts(&resp);
                }
                return Ok(());
            };
            let Some(location) = location else {
                let resp = client
                    .get(&format!(
                        "/organizations/{o}/business/google/locations?accountName={}",
                        urlencoding::encode(&account)
                    ))
                    .await
                    .context("GET GBP locations")?;
                if json {
                    print_json(&resp);
                } else {
                    print_locations(&resp);
                }
                return Ok(());
            };
            let resp = client
                .get(&format!(
                    "/organizations/{o}/business/google/reviews?accountName={}&locationName={}",
                    urlencoding::encode(&account),
                    urlencoding::encode(&location)
                ))
                .await
                .context("GET GBP reviews")?;
            if json {
                print_json(&resp);
            } else {
                print_reviews(&resp);
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

fn print_accounts(body: &serde_json::Value) {
    let Some(rows) = rows(body, "accounts") else {
        print_json(body);
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No Business Profile accounts connected yet.".dimmed());
        println!();
        return;
    }
    for a in rows {
        // `name` is the resource name (`accounts/123`) the next step wants;
        // `accountName` is the human-readable business name. Both, in that order.
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let title = a.get("accountName").and_then(|v| v.as_str()).unwrap_or("");
        let kind = a.get("type").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {} {}", "○".dimmed(), name.cyan(), title.bold(), format!("[{kind}]").dimmed());
    }
    println!();
    println!("  Next: smoo gbp reviews --account <name>");
    println!();
}

fn print_locations(body: &serde_json::Value) {
    let Some(rows) = rows(body, "locations") else {
        print_json(body);
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No locations on that account.".dimmed());
        println!();
        return;
    }
    for l in rows {
        let name = l.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let title = l.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let site = l.get("websiteUri").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {} {}", "○".dimmed(), name.cyan(), title.bold(), site.dimmed());
    }
    println!();
    println!("  Next: smoo gbp reviews --account <name> --location <name>");
    println!();
}

fn print_reviews(body: &serde_json::Value) {
    let Some(rows) = rows(body, "reviews") else {
        print_json(body);
        return;
    };
    println!();
    if rows.is_empty() {
        println!("  {} {}", "●".dimmed(), "No reviews on that location yet.".dimmed());
        println!();
        return;
    }
    for r in rows {
        let stars = r.get("starRating").and_then(|v| v.as_str()).unwrap_or("?");
        let reviewer = r
            .get("reviewer")
            .and_then(|v| v.get("displayName"))
            .and_then(|v| v.as_str())
            .unwrap_or("(anonymous)");
        let when = r.get("createTime").and_then(|v| v.as_str()).unwrap_or("");
        // Whether the org already answered is the one fact you need before
        // acting — surfaced so nobody drafts a second reply to the same review.
        let replied = if r.get("reviewReply").is_some_and(|v| !v.is_null()) {
            " [replied]"
        } else {
            ""
        };
        println!("  {} {} — {} {}{}", "○".dimmed(), stars.bold(), reviewer, when.dimmed(), replied.dimmed());
        if let Some(comment) = r.get("comment").and_then(|v| v.as_str()) {
            for line in comment.lines() {
                println!("      {line}");
            }
        }
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
    fn reviews_parses_bare() {
        let w = Wrap::try_parse_from(["t", "reviews"]).expect("bare reviews must parse");
        assert!(matches!(
            w.cmd,
            Cmd::Reviews {
                account: None,
                location: None,
                json: false,
                org: None
            }
        ));
    }

    #[test]
    fn reviews_parses_account_and_location() {
        let w = Wrap::try_parse_from(["t", "reviews", "--account", "accounts/1", "--location", "locations/2"]).expect("flags must parse");
        match w.cmd {
            Cmd::Reviews { account, location, .. } => {
                assert_eq!(account.as_deref(), Some("accounts/1"));
                assert_eq!(location.as_deref(), Some("locations/2"));
            }
        }
    }

    #[test]
    fn reviews_parses_json_and_org() {
        let w = Wrap::try_parse_from(["t", "reviews", "--json", "--org-id", "o1"]).expect("flags must parse");
        match w.cmd {
            Cmd::Reviews { json, org, .. } => {
                assert!(json);
                assert_eq!(org.as_deref(), Some("o1"));
            }
        }
    }
}
