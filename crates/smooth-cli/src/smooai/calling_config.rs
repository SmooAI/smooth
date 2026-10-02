//! `th smoo calling numbers|hours|settings …` — calling CONFIGURATION, the CLI
//! twin of the hosted MCP's `phone_numbers_list` / `phone_number_update`,
//! `business_hours_list` / `business_hours_update` and `calling_settings_get` /
//! `calling_settings_update` (SMOODEV-3612).
//!
//! Each verb wraps the same native api-prime route the dashboard's Settings →
//! Phone numbers / Calling screens call (`/organizations/{org}/calling/…`). The
//! route enforces the permission (`calling.use` to read, `calling.admin` to
//! write, `calling.recordings.manage` on top for the recording-compliance
//! settings) and the `telephony` product; a 403 is relayed as-is.
//!
//! Writes take `--set key=value` (repeatable; the value is parsed as JSON when it
//! is JSON — `true`, `30`, `null`, `[...]` — and taken as a string otherwise) or
//! `--body <file|->`. Keys outside the route's writable list are refused here by
//! name, with the list, rather than round-tripping a 400. Every write changes
//! what real callers get, so each asks for confirmation (`--yes` for scripts,
//! `--dry-run` to preview the body).

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{Map, Value};

use super::{print_json, read_body, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// `PATCHABLE` in smooai `rust/api-prime/src/handlers/calling/numbers.rs`.
pub const NUMBER_WRITABLE: &[&str] = &[
    "friendlyName",
    "purpose",
    "routeType",
    "routeUserId",
    "routeRingGroupId",
    "agentId",
    "forwardToE164",
    "fallbackType",
    "fallbackAgentId",
    "fallbackForwardE164",
    "aiPickupAfterSeconds",
    "businessHoursId",
    "afterHoursRouteType",
    "whisperEnabled",
    "whisperMessage",
    "recordingEnabled",
    "assignedUserId",
    "cnamDisplay",
    "voicemailGreetingUrl",
];
/// `WRITABLE` in `calling/business_hours.rs`.
pub const HOURS_WRITABLE: &[&str] = &["name", "timezone", "windows", "isDefault"];
/// `WRITABLE` in `calling/settings.rs`.
pub const SETTINGS_WRITABLE: &[&str] = &[
    "recordingConsentMode",
    "recordingRetentionDays",
    "announceOutbound",
    "ticketLookbackDays",
    "autoExpandSeats",
    "announcementClipUrl",
];

/// Shared write flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct Changes {
    /// A field to set, `key=value` (repeatable). Values that parse as JSON are
    /// sent as JSON (`true`, `30`, `null`, `[...]`); anything else as a string.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// A JSON object of fields from a file, or `-` for stdin.
    #[arg(long, conflicts_with = "set")]
    pub body: Option<String>,
}

#[derive(Subcommand)]
pub enum NumbersCmd {
    /// The org's phone numbers and how each one routes.
    #[command(visible_alias = "ls")]
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change how one number routes (needs calling admin). E.g.
    /// `--set routeType=ring_group --set routeRingGroupId=<id>`.
    Update {
        /// The number id (from `calling numbers list`).
        number_id: String,
        #[command(flatten)]
        changes: Changes,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

#[derive(Subcommand)]
pub enum HoursCmd {
    /// The org's business-hours schedules.
    #[command(visible_alias = "ls")]
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a schedule (needs calling admin): name, timezone, windows (the
    /// COMPLETE week, `[{"day":1,"startMinute":540,"endMinute":1020}, …]`), isDefault.
    Update {
        /// The schedule id (from `calling hours list`).
        hours_id: String,
        #[command(flatten)]
        changes: Changes,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

#[derive(Subcommand)]
pub enum SettingsCmd {
    /// The org's calling settings (recording consent, retention, announcements).
    Show {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change calling settings (needs calling admin; turning the recording
    /// announcement off or changing retention also needs recordings-manage).
    Update {
        #[command(flatten)]
        changes: Changes,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// An id destined for a URL path segment (same rule as the MCP's `path_id`).
fn path_id(label: &str, id: &str) -> Result<String> {
    let id = id.trim();
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("{label} must be a Smoo id (letters, digits, `-`, `_`); got {id:?}");
    }
    Ok(id.to_string())
}

/// `--set` pairs as a JSON object.
fn parse_sets(sets: &[String]) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for pair in sets {
        let Some((k, v)) = pair.split_once('=') else {
            bail!("--set takes key=value; got {pair:?}");
        };
        let k = k.trim();
        if k.is_empty() {
            bail!("--set has an empty key: {pair:?}");
        }
        let value = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        out.insert(k.to_string(), value);
    }
    Ok(out)
}

/// The PATCH body, refusing any key outside `writable` by name.
pub fn checked(what: &str, changes: Map<String, Value>, writable: &[&str]) -> Result<Value> {
    if changes.is_empty() {
        bail!(
            "nothing to change — pass --set key=value or --body (writable {what} fields: {})",
            writable.join(", ")
        );
    }
    if let Some(k) = changes.keys().find(|k| !writable.contains(&k.as_str())) {
        bail!("{k:?} is not a writable {what} field. Writable: {}", writable.join(", "));
    }
    Ok(Value::Object(changes))
}

fn body_from(changes: &Changes, what: &str, writable: &[&str]) -> Result<Value> {
    let map = match &changes.body {
        Some(path) => match read_body(path)? {
            Value::Object(m) => m,
            _ => bail!("--body must be a JSON object"),
        },
        None => parse_sets(&changes.set)?,
    };
    checked(what, map, writable)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn rows(body: &Value) -> Vec<&Value> {
    body.as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

/// The number's route target, whichever column its route type uses.
fn target(n: &Value) -> String {
    let key = match s(n, "routeType") {
        "user" => "routeUserId",
        "ring_group" => "routeRingGroupId",
        "ai_agent" | "ai_ivr" => "agentId",
        "forward" => "forwardToE164",
        _ => return String::new(),
    };
    match s(n, key) {
        "" => String::new(),
        t => format!(" → {t}"),
    }
}

fn render_numbers(body: &Value) {
    let list = rows(body);
    println!();
    if list.is_empty() {
        println!("  {} {}", "●".dimmed(), "no phone numbers".dimmed());
    }
    for n in list {
        let name = match s(n, "friendlyName") {
            "" => String::new(),
            f => format!(" {f}"),
        };
        println!("  {} {}{} {}", "○".dimmed(), s(n, "e164").bold(), name, format!("({})", s(n, "id")).dimmed());
        println!("      route {}{}", s(n, "routeType").cyan(), target(n));
        if !s(n, "businessHoursId").is_empty() {
            println!("      hours {} · after hours {}", s(n, "businessHoursId"), s(n, "afterHoursRouteType"));
        }
        if !s(n, "releasedAt").is_empty() {
            println!("      {}", "released (read-only)".yellow());
        }
    }
    println!();
}

fn render_hours(body: &Value) {
    let list = rows(body);
    println!();
    if list.is_empty() {
        println!("  {} {}", "●".dimmed(), "no business-hours schedules".dimmed());
    }
    for h in list {
        let default = if h.get("isDefault").and_then(Value::as_bool) == Some(true) {
            " [default]"
        } else {
            ""
        };
        let windows = h.get("windows").and_then(Value::as_array).map_or(0, Vec::len);
        println!(
            "  {} {} {}{} — {}, {} window(s)",
            "○".dimmed(),
            s(h, "name").bold(),
            format!("({})", s(h, "id")).dimmed(),
            default.green(),
            s(h, "timezone"),
            windows
        );
    }
    println!();
}

async fn write(path: &str, body: Value, noun: &str, id: &str, org: &str, confirm: Confirm) -> Result<()> {
    if confirm.dry_run {
        println!("\n  {} would PATCH {path}", "●".dimmed());
        print_json(&body);
    }
    let proceed = crate::destructive::gate_with(
        &Target {
            verb: "change",
            noun,
            id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )?;
    if proceed {
        let client = require_authed().await?;
        let updated = client.patch(path, &body).await.with_context(|| format!("PATCH {path} (needs calling admin)"))?;
        println!("\n  {} updated", "✓".green());
        print_json(&updated);
    }
    Ok(())
}

pub async fn numbers(cmd: NumbersCmd) -> Result<()> {
    match cmd {
        NumbersCmd::List { org, json } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/numbers?limit=200"))
                .await
                .context("GET calling/numbers")?;
            if json {
                print_json(&body);
            } else {
                render_numbers(&body);
            }
        }
        NumbersCmd::Update {
            number_id,
            changes,
            org,
            confirm,
        } => {
            let id = path_id("number id", &number_id)?;
            let body = body_from(&changes, "phone number", NUMBER_WRITABLE)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            write(
                &format!("/organizations/{o}/calling/numbers/{id}"),
                body,
                "phone number routing",
                &id,
                &o,
                confirm,
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn hours(cmd: HoursCmd) -> Result<()> {
    match cmd {
        HoursCmd::List { org, json } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/business-hours?limit=200"))
                .await
                .context("GET calling/business-hours")?;
            if json {
                print_json(&body);
            } else {
                render_hours(&body);
            }
        }
        HoursCmd::Update {
            hours_id,
            changes,
            org,
            confirm,
        } => {
            let id = path_id("schedule id", &hours_id)?;
            let body = body_from(&changes, "business hours", HOURS_WRITABLE)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            write(
                &format!("/organizations/{o}/calling/business-hours/{id}"),
                body,
                "business-hours schedule",
                &id,
                &o,
                confirm,
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn settings(cmd: SettingsCmd) -> Result<()> {
    match cmd {
        SettingsCmd::Show { org, json: _ } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .get(&format!("/organizations/{o}/calling/settings"))
                    .await
                    .context("GET calling/settings")?,
            );
        }
        SettingsCmd::Update { changes, org, confirm } => {
            let body = body_from(&changes, "calling settings", SETTINGS_WRITABLE)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            write(
                &format!("/organizations/{o}/calling/settings"),
                body,
                "calling settings",
                "calling",
                &o,
                confirm,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sets_parse_json_values_and_fall_back_to_strings() {
        let m = parse_sets(&[
            "recordingEnabled=true".into(),
            "aiPickupAfterSeconds=20".into(),
            "friendlyName=Main line".into(),
            "businessHoursId=null".into(),
            "windows=[{\"day\":1,\"startMinute\":540,\"endMinute\":1020}]".into(),
        ])
        .unwrap();
        assert_eq!(m["recordingEnabled"], json!(true));
        assert_eq!(m["aiPickupAfterSeconds"], json!(20));
        assert_eq!(m["friendlyName"], json!("Main line"));
        assert_eq!(m["businessHoursId"], Value::Null);
        assert_eq!(m["windows"][0]["day"], json!(1));
        assert!(parse_sets(&["novalue".into()]).is_err());
        assert!(parse_sets(&["=x".into()]).is_err());
    }

    #[test]
    fn keys_outside_the_route_allowlist_are_refused_by_name() {
        let mut m = Map::new();
        m.insert("twilioSid".into(), json!("PN1"));
        let err = checked("phone number", m, NUMBER_WRITABLE).unwrap_err().to_string();
        assert!(err.contains("twilioSid") && err.contains("friendlyName"), "{err}");
        assert!(checked("calling settings", Map::new(), SETTINGS_WRITABLE).is_err());
        let mut ok = Map::new();
        ok.insert("autoExpandSeats".into(), json!(false));
        assert_eq!(checked("calling settings", ok, SETTINGS_WRITABLE).unwrap(), json!({ "autoExpandSeats": false }));
    }

    #[test]
    fn route_targets_follow_the_route_type() {
        assert_eq!(target(&json!({ "routeType": "ring_group", "routeRingGroupId": "g1" })), " → g1");
        assert_eq!(target(&json!({ "routeType": "forward", "forwardToE164": "+13175550142" })), " → +13175550142");
        assert_eq!(target(&json!({ "routeType": "ai_agent" })), "");
        assert!(path_id("number id", "../x").is_err());
    }
}
