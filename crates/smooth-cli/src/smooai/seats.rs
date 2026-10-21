//! `th smoo calling seats list|assign|unassign` — calling seats (SMOODEV-4052),
//! the CLI twin of the hosted MCP's `calling_seats_list` / `calling_seats_assign`
//! / `calling_seats_unassign` and the copilot's `calling_seats.*`.
//!
//! Only seat holders can take or place human calls. Every verb calls the
//! api-prime route the Calling settings page calls (`/organizations/{org}/calling/seats…`)
//! with your own sign-in: `calling.use` to read, `calling.admin` to write, plus
//! the `telephony` product. A 403 is reported as "admin only".
//!
//! `assign` keeps the route's 402 when every seat is taken. If the org turned on
//! auto-expand seats, the route instead grows the paid seat plan by one and the
//! org is billed, so the confirmation says so.

use std::fmt::Write as _;

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::ring_groups::{admin_only, group_id as uuid_id};
use super::{print_json, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

#[derive(Subcommand)]
pub enum SeatsCmd {
    /// Who holds a calling seat, and the org's seat capacity.
    #[command(visible_alias = "ls")]
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Give an org member a calling seat (needs calling admin). Refused when every
    /// seat is taken — unless the org has auto-expand on, which grows the paid
    /// seat plan by one (billed).
    #[command(visible_alias = "add")]
    Assign {
        /// The member's user id.
        user_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Take a calling seat back (needs calling admin): the member stops ringing and
    /// their phone sessions end. Pass the user id, or `--seat-id`.
    #[command(visible_alias = "rm")]
    Unassign {
        /// The seat holder's user id.
        #[arg(required_unless_present = "seat_id", conflicts_with = "seat_id")]
        user_id: Option<String>,
        /// The seat id (from `calling seats list`).
        #[arg(long = "seat-id")]
        seat_id: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// The capacity line: "1 of 5 seat(s) assigned (operator grant 5, paid 0)".
#[must_use]
pub fn capacity_line(body: &Value) -> String {
    let cap = body.get("capacity").cloned().unwrap_or(Value::Null);
    format!(
        "{} of {} seat(s) assigned (operator grant {}, paid {})",
        n(body, "used"),
        n(&cap, "total"),
        n(&cap, "overrideSeats"),
        n(&cap, "paidSeats")
    )
}

/// The active seat a user holds in a seat list body.
#[must_use]
pub fn seat_for_user<'a>(body: &'a Value, user_id: &str) -> Option<&'a Value> {
    body.get("seats")?.as_array()?.iter().find(|seat| s(seat, "userId") == user_id)
}

/// What `assign` reports from the route's answer.
#[must_use]
pub fn assign_summary(user: &str, r: &Value) -> String {
    let mut out = if r.get("created").and_then(Value::as_bool) == Some(false) {
        format!("{user} already holds a seat — nothing changed")
    } else {
        format!("seat assigned to {user}")
    };
    let total = r.get("capacity").map_or(0, |c| n(c, "total"));
    let _ = write!(out, "; {} of {total} seat(s) now assigned", n(r, "used"));
    if r.get("autoExpanded").and_then(Value::as_bool) == Some(true) {
        out.push_str("; the paid seat plan grew by one to make room (auto-expand)");
    }
    out
}

fn user_id(id: &str) -> Result<String> {
    uuid_id(id).map_err(|_| anyhow::anyhow!("user id must be a uuid; got {:?}", id.trim()))
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb
pub async fn seats(cmd: SeatsCmd) -> Result<()> {
    match cmd {
        SeatsCmd::List { org, json } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/seats"))
                .await
                .map_err(|e| admin_only(e, false))
                .context("GET calling/seats")?;
            if json {
                print_json(&body);
                return Ok(());
            }
            println!("\n  {}", capacity_line(&body).bold());
            let list = body.get("seats").and_then(Value::as_array).cloned().unwrap_or_default();
            if list.is_empty() {
                println!("  {} {}", "●".dimmed(), "nobody holds a seat".dimmed());
            }
            for seat in &list {
                println!(
                    "  {} {} {} — {}, since {}",
                    "○".dimmed(),
                    s(seat, "userId"),
                    format!("(seat {})", s(seat, "id")).dimmed(),
                    s(seat, "source"),
                    s(seat, "assignedAt")
                );
            }
            println!();
        }
        SeatsCmd::Assign { user_id: user, org, confirm } => {
            let user = user_id(&user)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let path = format!("/organizations/{o}/calling/seats");
            let body = json!({ "userId": user });
            if confirm.dry_run {
                println!("\n  {} would POST {path}", "●".dimmed());
                print_json(&body);
            }
            println!(
                "\n  {} if every seat is taken and the org has auto-expand on, this adds a paid seat",
                "note".yellow()
            );
            let target = Target {
                verb: "assign a calling seat to",
                noun: "user",
                id: &user,
                org: &o,
                severity: Severity::Standard,
            };
            if crate::destructive::gate_with(&target, confirm)? {
                let r = client
                    .post(&path, Some(&body))
                    .await
                    .map_err(|e| admin_only(e, true))
                    .with_context(|| format!("POST {path} (402 = every seat is taken: buy more or unassign one)"))?;
                println!("\n  {} {}\n", "✓".green(), assign_summary(&user, &r));
            }
        }
        SeatsCmd::Unassign {
            user_id: user,
            seat_id,
            org,
            confirm,
        } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let seat = match (seat_id, user) {
                (Some(seat), _) => uuid_id(&seat).map_err(|_| anyhow::anyhow!("--seat-id must be a uuid (from `calling seats list`)"))?,
                (None, Some(user)) => {
                    let user = user_id(&user)?;
                    let list = client
                        .get(&format!("/organizations/{o}/calling/seats"))
                        .await
                        .map_err(|e| admin_only(e, false))
                        .context("GET calling/seats")?;
                    let Some(id) = seat_for_user(&list, &user).map(|seat| s(seat, "id").to_string()).filter(|id| !id.is_empty()) else {
                        anyhow::bail!("user {user} does not hold a calling seat — nothing to unassign");
                    };
                    id
                }
                (None, None) => anyhow::bail!("pass the seat holder's user id or --seat-id"),
            };
            let target = Target {
                verb: "unassign",
                noun: "calling seat",
                id: &seat,
                org: &o,
                severity: Severity::Standard,
            };
            if crate::destructive::gate_with(&target, confirm)? {
                let path = format!("/organizations/{o}/calling/seats/{seat}");
                let r = client
                    .delete(&path)
                    .await
                    .map_err(|e| admin_only(e, true))
                    .with_context(|| format!("DELETE {path}"))?;
                println!("\n  {} seat {seat} unassigned from {}\n", "✓".green(), s(&r, "userId"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const U1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const SEAT: &str = "bbbbbbbb-0000-0000-0000-000000000001";

    #[test]
    fn capacity_and_lookup() {
        let body = json!({ "capacity": { "total": 5, "overrideSeats": 5, "paidSeats": 0 }, "used": 1, "seats": [{ "id": SEAT, "userId": U1 }] });
        assert_eq!(capacity_line(&body), "1 of 5 seat(s) assigned (operator grant 5, paid 0)");
        assert_eq!(seat_for_user(&body, U1).unwrap()["id"], json!(SEAT));
        assert!(seat_for_user(&body, "someone-else").is_none());
        assert!(seat_for_user(&json!({}), U1).is_none());
    }

    #[test]
    fn assign_summary_flags_auto_expand_and_no_ops() {
        let r = json!({ "created": true, "used": 3, "capacity": { "total": 3 }, "autoExpanded": true });
        let out = assign_summary(U1, &r);
        assert!(out.contains("3 of 3") && out.contains("auto-expand"), "{out}");
        let r = json!({ "created": false, "used": 2, "capacity": { "total": 3 }, "autoExpanded": false });
        assert!(assign_summary(U1, &r).contains("nothing changed"));
    }

    #[test]
    fn ids_must_be_uuids() {
        assert!(user_id("../plan").is_err());
        assert_eq!(user_id(U1).unwrap(), U1);
    }
}
