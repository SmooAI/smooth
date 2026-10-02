//! `th smoo calling …` — the CLI twin of the hosted MCP's calling tools
//! (`calls_list`, `calls_get`, `voicemails_list`, `calling_usage`,
//! `call_place`; SMOODEV-3545).
//!
//! Every read is a thin wrapper over the same `api.smoo.ai` route the MCP tool
//! calls (`/organizations/{org}/calling/…`), with the MCP's client-side guards
//! mirrored: ids bound for a URL path must look like Smoo ids, a page size is
//! clamped to what the route accepts, and a phone number is normalised to
//! E.164 or refused — never guessed, because a guessed number reaches a
//! stranger.
//!
//! `place` contacts a real person, so it is gated harder than anything else
//! here. Like the MCP tool it never dials on its own: it reads `calling/me`
//! (can this rep dial right now, and if not why), asks for confirmation (type
//! the number back, `--yes` for scripts, `--dry-run` to preview), and then
//! hands off a dashboard link that opens the rep's softphone with the call
//! READY. The rep presses Call there, and the softphone runs the usual checks
//! (caller ID, do-not-call, quiet hours) before anything rings. No request in
//! this file creates a call, so there is nothing to retry.

use anstream::{eprintln, println};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::Value;

use super::{print_json, require_active_org, require_authed, require_user_session};
use crate::destructive::{Confirm, Severity, Target};

/// Statuses the calls list route filters on (comma-separated upstream).
const CALL_STATUSES: &[&str] = &[
    "ringing",
    "ai_handling",
    "in_progress",
    "on_hold",
    "completed",
    "missed",
    "voicemail",
    "failed",
    "blocked",
];

/// Voicemails per call — the list route carries each one's transcript.
const MAX_VOICEMAILS: u32 = 25;

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's phone calls, newest first.
    #[command(visible_alias = "ls")]
    List {
        /// `inbound` or `outbound`.
        #[arg(long)]
        direction: Option<String>,
        /// One or more of ringing, ai_handling, in_progress, on_hold, completed,
        /// missed, voicemail, failed, blocked (comma-separated).
        #[arg(long)]
        status: Option<String>,
        /// Only calls linked to this CRM contact id.
        #[arg(long = "contact-id", visible_alias = "contact")]
        contact_id: Option<String>,
        /// Calls that started at or after this instant (RFC3339, e.g. 2026-09-01T00:00:00Z).
        #[arg(long)]
        since: Option<String>,
        /// Calls that started before this instant (RFC3339).
        #[arg(long)]
        until: Option<String>,
        /// Only calls you answered or had a leg on (user session).
        #[arg(long)]
        mine: bool,
        /// Page size, 1–100 (default 25).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: Option<u32>,
        /// The cursor a previous page printed.
        #[arg(long)]
        cursor: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// One call in full: summary, notes, action items, signals, voicemail.
    Get {
        /// Call id (from `calling list`).
        call_id: String,
        /// Also fetch the transcript (user session; someone else's call also
        /// needs transcript access).
        #[arg(long)]
        transcript: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Recent voicemails with their transcripts, newest first.
    #[command(visible_alias = "voicemail")]
    Voicemails {
        /// Voicemails left at or after this instant (RFC3339).
        #[arg(long)]
        since: Option<String>,
        /// How many, 1–25 (default 10).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=25))]
        limit: Option<u32>,
        /// Only voicemails nobody has listened to yet.
        #[arg(long)]
        unheard: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the matching call rows as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Phone minutes used this month against the included allowance
    /// (needs calling admin or billing access).
    Usage {
        /// A calendar month `YYYY-MM` (Eastern time). Default: this month.
        #[arg(long)]
        period: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
    },
    /// Place an outbound call from YOUR Smoo softphone. Contacts a real person.
    ///
    /// Checks whether you can dial right now, asks you to confirm by typing the
    /// number back (`--yes` for scripts, `--dry-run` to only check), then opens
    /// the Phone page with the call ready. Nothing rings until you press Call
    /// there; the softphone runs the caller-ID, do-not-call and quiet-hours
    /// checks first. Needs a signed-in user (`smoo auth login`), not an org key.
    Place {
        /// The number: E.164 (`+13175550142`) or a 10-digit US number.
        to: String,
        /// The CRM contact the call is for; links the call.
        #[arg(long = "contact-id", visible_alias = "contact")]
        contact_id: Option<String>,
        /// The contact's name, shown on the confirmation.
        #[arg(long = "contact-name", visible_alias = "name")]
        contact_name: Option<String>,
        /// Print the link instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Phone numbers and their routing — list, or change one (SMOODEV-3612).
    #[command(visible_alias = "number")]
    Numbers {
        #[command(subcommand)]
        cmd: super::calling_config::NumbersCmd,
    },
    /// Business-hours schedules — list, or change one (SMOODEV-3612).
    #[command(visible_alias = "business-hours")]
    Hours {
        #[command(subcommand)]
        cmd: super::calling_config::HoursCmd,
    },
    /// Calling settings: recording consent, retention, announcements (SMOODEV-3612).
    Settings {
        #[command(subcommand)]
        cmd: super::calling_config::SettingsCmd,
    },
}

/// An id destined for a URL path segment. Smoo ids are UUIDs; anything with a
/// `/`, `?`, `#` or whitespace could re-point the request at another route, so
/// it is refused rather than encoded (same rule as the MCP's `path_id`).
fn path_id(label: &str, id: &str) -> Result<String> {
    let id = id.trim();
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("{label} must be a Smoo id (letters, digits, `-`, `_`); got {id:?}");
    }
    Ok(id.to_string())
}

/// A number as E.164: `+` and 8–15 digits, or a 10-digit US number (optional
/// leading 1). Spaces, dashes, dots and parentheses are ignored; anything else
/// is refused rather than guessed. Mirrors the MCP's `dial_e164`.
fn dial_e164(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    let junk = trimmed
        .strip_prefix('+')
        .unwrap_or(trimmed)
        .chars()
        .any(|c| !(c.is_ascii_digit() || " ()-.".contains(c)));
    let e164 = if junk {
        None
    } else if trimmed.starts_with('+') {
        (8..=15).contains(&digits.len()).then(|| format!("+{digits}"))
    } else if digits.len() == 10 {
        Some(format!("+1{digits}"))
    } else if digits.len() == 11 && digits.starts_with('1') {
        Some(format!("+{digits}"))
    } else {
        None
    };
    e164.ok_or_else(|| anyhow::anyhow!("the number must be E.164 (+13175550142) or a 10-digit US number; got {raw:?}"))
}

/// `inbound` / `outbound`, case-insensitive.
fn direction(raw: &str) -> Result<String> {
    let d = raw.trim().to_ascii_lowercase();
    if d == "inbound" || d == "outbound" {
        Ok(d)
    } else {
        bail!("--direction must be `inbound` or `outbound`; got {raw:?}")
    }
}

/// A comma-separated status filter, each one from [`CALL_STATUSES`].
fn statuses(raw: &str) -> Result<String> {
    let parts: Vec<String> = raw.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        bail!("--status is empty — use one or more of: {}", CALL_STATUSES.join(", "));
    }
    if let Some(bad) = parts.iter().find(|s| !CALL_STATUSES.contains(&s.as_str())) {
        bail!("unknown status {bad:?} — use one or more of: {}", CALL_STATUSES.join(", "));
    }
    Ok(parts.join(","))
}

/// `YYYY-MM` with a real month.
fn period(raw: &str) -> Result<String> {
    let p = raw.trim();
    let ok = p.len() == 7 && p.as_bytes()[4] == b'-' && p[..4].chars().all(|c| c.is_ascii_digit()) && p[5..].parse::<u8>().is_ok_and(|m| (1..=12).contains(&m));
    if !ok {
        bail!("--period must be a month like 2026-09; got {raw:?}");
    }
    Ok(p.to_string())
}

/// Append `key=value` (URL-encoded) to a query string when the value is set.
fn push_q(q: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
        q.push(format!("{key}={}", urlencoding::encode(v)));
    }
}

fn with_query(base: String, q: &[String]) -> String {
    if q.is_empty() {
        base
    } else {
        format!("{base}?{}", q.join("&"))
    }
}

/// Filters for `GET calling/calls`.
#[derive(Debug, Default)]
struct ListFilter<'a> {
    direction: Option<&'a str>,
    status: Option<&'a str>,
    contact_id: Option<&'a str>,
    since: Option<&'a str>,
    until: Option<&'a str>,
    mine: bool,
    limit: Option<u32>,
    cursor: Option<&'a str>,
}

/// `GET /organizations/{org}/calling/calls?…` — the MCP's param names
/// (`contactId`, `from`, `to`, `mine`, `limit` default 25, `cursor`).
fn list_path(org: &str, f: &ListFilter) -> Result<String> {
    let mut q = Vec::new();
    push_q(&mut q, "direction", f.direction.map(direction).transpose()?.as_deref());
    push_q(&mut q, "status", f.status.map(statuses).transpose()?.as_deref());
    push_q(&mut q, "contactId", f.contact_id.map(|c| path_id("--contact-id", c)).transpose()?.as_deref());
    push_q(&mut q, "from", f.since);
    push_q(&mut q, "to", f.until);
    if f.mine {
        q.push("mine=true".to_string());
    }
    q.push(format!("limit={}", f.limit.unwrap_or(25).clamp(1, 100)));
    push_q(&mut q, "cursor", f.cursor);
    Ok(with_query(format!("/organizations/{org}/calling/calls"), &q))
}

/// `GET calling/calls?status=voicemail…`. `--unheard` has no route filter, so
/// over-read the page (×4, ≤100) and skip the heard ones here, as the MCP does.
fn voicemails_path(org: &str, since: Option<&str>, limit: u32, unheard: bool) -> String {
    let page = if unheard { (limit * 4).min(100) } else { limit };
    let mut q = vec!["status=voicemail".to_string()];
    push_q(&mut q, "from", since);
    q.push(format!("limit={page}"));
    with_query(format!("/organizations/{org}/calling/calls"), &q)
}

fn usage_path(org: &str, period_arg: Option<&str>) -> Result<String> {
    let mut q = Vec::new();
    push_q(&mut q, "period", period_arg.map(period).transpose()?.as_deref());
    Ok(with_query(format!("/organizations/{org}/calling/usage"), &q))
}

/// The dashboard link that opens the Phone page with this call ready (the
/// MCP's `dial_link`). The rep's softphone places it after they press Call.
fn dial_link(web: &str, to: &str, contact_id: Option<&str>) -> String {
    let mut url = format!("{}/apps/phone?dial={}", web.trim_end_matches('/'), to.replace('+', "%2B"));
    if let Some(id) = contact_id {
        url.push_str(&format!("&contactId={id}"));
    }
    url
}

/// From `GET calling/me`: `Ok(())` when this rep can dial, else the reasons.
fn can_dial(me: &Value) -> std::result::Result<(), String> {
    if me.get("canDial").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(());
    }
    let reasons: Vec<&str> = me
        .get("reasons")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Err(if reasons.is_empty() {
        "outbound calling is not switched on for this organization".to_string()
    } else {
        format!("reasons: {}", reasons.join(", "))
    })
}

/// The `calls` rows of a list body.
fn rows(body: &Value) -> &[Value] {
    body.get("calls").and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
}

/// A call row that left a voicemail (and, with `unheard`, nobody played it).
fn is_voicemail(call: &Value, unheard: bool) -> bool {
    let has = call.get("voicemailId").is_some_and(|v| !v.is_null());
    let heard = call.get("voicemailHeardAt").is_some_and(|h| !h.is_null());
    has && !(unheard && heard)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn render_calls(body: &Value) {
    let calls = rows(body);
    println!();
    if calls.is_empty() {
        println!("  {} {}", "●".dimmed(), "no calls matched".dimmed());
    }
    for c in calls {
        let who = [s(c, "contactName"), s(c, "companyName")].into_iter().find(|x| !x.is_empty()).unwrap_or("");
        let number = if s(c, "direction") == "inbound" { s(c, "fromE164") } else { s(c, "toE164") };
        let talk = c.get("talkSeconds").and_then(Value::as_u64).map(|t| format!(" {t}s")).unwrap_or_default();
        println!(
            "  {} {} {} {} {}{}",
            if s(c, "direction") == "inbound" { "↙" } else { "↗" },
            s(c, "id").cyan(),
            format!("[{}]", s(c, "status")).dimmed(),
            number.bold(),
            who,
            format!("  {}{talk}", s(c, "startedAt")).dimmed()
        );
    }
    println!();
    if let Some(next) = body.get("nextCursor").and_then(Value::as_str) {
        eprintln!("  {} more calls: --cursor {next}", "●".dimmed());
    }
}

fn render_voicemails(calls: &[&Value]) {
    println!();
    for c in calls {
        let heard = c.get("voicemailHeardAt").is_some_and(|h| !h.is_null());
        let dur = c
            .get("voicemailDurationSeconds")
            .filter(|d| !d.is_null())
            .map_or("?".to_string(), ToString::to_string);
        let transcript = s(c, "voicemailTranscript").trim();
        println!(
            "  {} {} {} {} {}",
            if heard { "○".dimmed().to_string() } else { "●".yellow().to_string() },
            s(c, "id").cyan(),
            s(c, "fromE164").bold(),
            s(c, "contactName"),
            format!("{}  {dur}s  {}", s(c, "startedAt"), if heard { "heard" } else { "NOT heard" }).dimmed()
        );
        println!("      {}", if transcript.is_empty() { "(no transcript)" } else { transcript });
    }
    println!();
}

fn render_usage(body: &Value) {
    let n = |k: &str| body.get(k).filter(|v| !v.is_null()).map_or("—".to_string(), ToString::to_string);
    println!();
    if let Some(p) = body.get("period") {
        println!("  {} {}", "period".dimmed(), s(p, "key").bold());
    }
    println!(
        "  {} {} min over {} calls ({}%)",
        "used  ".dimmed(),
        n("usedMinutes"),
        n("calls"),
        n("percentUsed")
    );
    if let Some(a) = body.get("allowance") {
        let inc = a.get("includedMinutes").map_or("—".to_string(), ToString::to_string);
        println!("  {} {inc} min included", "allow ".dimmed());
    }
    println!("  {} {} min by month end", "proj  ".dimmed(), n("projectedMinutes"));
    println!(
        "  {} {} min, est. {}¢ (projected {}¢)",
        "over  ".dimmed(),
        n("overageMinutes"),
        n("estimatedOverageCents"),
        n("projectedOverageCents")
    );
    println!();
    println!("  {}", "--json for the by-lane and by-rep breakdown".dimmed());
    println!();
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb
pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Numbers { cmd } => return super::calling_config::numbers(cmd).await,
        Cmd::Hours { cmd } => return super::calling_config::hours(cmd).await,
        Cmd::Settings { cmd } => return super::calling_config::settings(cmd).await,
        Cmd::List {
            direction,
            status,
            contact_id,
            since,
            until,
            mine,
            limit,
            cursor,
            org,
            json,
        } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let path = list_path(
                &o,
                &ListFilter {
                    direction: direction.as_deref(),
                    status: status.as_deref(),
                    contact_id: contact_id.as_deref(),
                    since: since.as_deref(),
                    until: until.as_deref(),
                    mine,
                    limit,
                    cursor: cursor.as_deref(),
                },
            )?;
            let body = client.get(&path).await.context("GET calling/calls")?;
            if json {
                print_json(&body);
            } else {
                render_calls(&body);
            }
        }
        Cmd::Get { call_id, transcript, org } => {
            let id = path_id("call id", &call_id)?;
            // The transcript route is user-only; ask for the session up front
            // rather than fetching the call and then failing.
            let client = if transcript {
                require_user_session()
                    .await
                    .context("--transcript needs a user session — run `smoo auth login`")?
            } else {
                require_authed().await?
            };
            let o = require_active_org(&client, org)?;
            let mut call = client
                .get(&format!("/organizations/{o}/calling/calls/{id}"))
                .await
                .context("GET calling/calls/{id}")?;
            if transcript {
                let t = client
                    .get(&format!("/organizations/{o}/calling/calls/{id}/transcript"))
                    .await
                    .context("GET calling/calls/{id}/transcript")?;
                if let Value::Object(m) = &mut call {
                    m.insert("transcript".to_string(), t);
                } else {
                    call = serde_json::json!({ "call": call, "transcript": t });
                }
            }
            print_json(&call);
        }
        Cmd::Voicemails {
            since,
            limit,
            unheard,
            org,
            json,
        } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let limit = limit.unwrap_or(10).clamp(1, MAX_VOICEMAILS);
            let body = client
                .get(&voicemails_path(&o, since.as_deref(), limit, unheard))
                .await
                .context("GET calling/calls?status=voicemail")?;
            let picked: Vec<&Value> = rows(&body).iter().filter(|c| is_voicemail(c, unheard)).take(limit as usize).collect();
            if json {
                print_json(&Value::Array(picked.into_iter().cloned().collect()));
            } else if picked.is_empty() {
                println!();
                let what = if unheard { "no unheard voicemails" } else { "no voicemails matched" };
                println!("  {} {}", "●".dimmed(), what.dimmed());
                println!();
            } else {
                render_voicemails(&picked);
            }
        }
        Cmd::Usage { period, org, json } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&usage_path(&o, period.as_deref())?)
                .await
                .context("GET calling/usage (needs calling admin or billing access)")?;
            if json {
                print_json(&body);
            } else {
                render_usage(&body);
            }
        }
        Cmd::Place {
            to,
            contact_id,
            contact_name,
            no_browser,
            org,
            confirm,
        } => {
            // Validate everything locally before touching the network.
            let to = dial_e164(&to)?;
            let contact = contact_id.as_deref().map(|c| path_id("--contact-id", c)).transpose()?;
            let client = require_user_session()
                .await
                .context("placing a call needs a signed-in user (your softphone) — run `smoo auth login`; an org API key cannot dial")?;
            let o = require_active_org(&client, org)?;
            let me = client.get(&format!("/organizations/{o}/calling/me")).await.context("GET calling/me")?;
            let who = contact_name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map_or_else(|| to.clone(), |n| format!("{n} at {to}"));
            if let Err(why) = can_dial(&me) {
                bail!("you cannot place calls from Smoo right now, so nothing was prepared for {who} — {why}");
            }
            // Banner reads "about to place phone call to Pat at +1317…"; the
            // Irreversible prompt makes the operator type the number back.
            let name = contact_name.as_deref().map(str::trim).filter(|n| !n.is_empty());
            let noun = name.map_or_else(|| "phone call to".to_string(), |n| format!("phone call to {n} at"));
            let proceed = crate::destructive::gate_with(
                &Target {
                    verb: "place",
                    noun: &noun,
                    id: &to,
                    org: &o,
                    severity: Severity::Irreversible,
                },
                confirm,
            )?;
            if proceed {
                let link = dial_link(&super::web_url(), &to, contact.as_deref());
                println!("  {} call to {who} is ready — press Call in your softphone", "☎".green());
                println!("  {}", "caller ID, do-not-call and quiet-hours checks run before anything rings".dimmed());
                println!("  {link}");
                if !no_browser {
                    if let Err(e) = open::that(&link) {
                        eprintln!("  (couldn't auto-open the browser: {e}. Open the link above to continue.)");
                    }
                }
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
    fn every_verb_parses() {
        assert!(matches!(
            parse(&[
                "list",
                "--direction",
                "inbound",
                "--status",
                "missed,voicemail",
                "--mine",
                "--limit",
                "5",
                "--json"
            ])
            .unwrap(),
            Cmd::List {
                mine: true,
                json: true,
                limit: Some(5),
                ..
            }
        ));
        assert!(matches!(parse(&["ls"]).unwrap(), Cmd::List { mine: false, .. }));
        assert!(matches!(parse(&["get", "c1", "--transcript"]).unwrap(), Cmd::Get { transcript: true, .. }));
        assert!(matches!(
            parse(&["voicemails", "--unheard", "--limit", "3"]).unwrap(),
            Cmd::Voicemails {
                unheard: true,
                limit: Some(3),
                ..
            }
        ));
        assert!(matches!(parse(&["usage", "--period", "2026-09"]).unwrap(), Cmd::Usage { .. }));
        assert!(matches!(parse(&["place", "3175550142"]).unwrap(), Cmd::Place { .. }));
        // SMOODEV-3612 — the configuration verbs.
        assert!(matches!(parse(&["numbers", "list"]).unwrap(), Cmd::Numbers { .. }));
        assert!(matches!(
            parse(&[
                "numbers",
                "update",
                "n1",
                "--set",
                "routeType=ring_group",
                "--set",
                "routeRingGroupId=g1",
                "--dry-run"
            ])
            .unwrap(),
            Cmd::Numbers { .. }
        ));
        assert!(
            parse(&["numbers", "update", "n1", "--set", "a=b", "--body", "-"]).is_err(),
            "--set and --body conflict"
        );
        assert!(matches!(parse(&["hours", "ls"]).unwrap(), Cmd::Hours { .. }));
        assert!(matches!(
            parse(&["business-hours", "update", "h1", "--set", "name=Main"]).unwrap(),
            Cmd::Hours { .. }
        ));
        assert!(matches!(parse(&["settings", "show"]).unwrap(), Cmd::Settings { .. }));
        assert!(matches!(
            parse(&["settings", "update", "--set", "autoExpandSeats=false", "--yes"]).unwrap(),
            Cmd::Settings { .. }
        ));
    }

    #[test]
    fn limits_are_bounded_by_clap() {
        assert!(parse(&["list", "--limit", "0"]).is_err());
        assert!(parse(&["list", "--limit", "101"]).is_err());
        assert!(parse(&["voicemails", "--limit", "26"]).is_err());
        assert!(parse(&["voicemails", "--limit", "25"]).is_ok());
    }

    #[test]
    fn place_confirms_by_default() {
        match parse(&["place", "+13175550142"]).unwrap() {
            Cmd::Place { confirm, no_browser, .. } => {
                assert!(!confirm.yes && !confirm.dry_run, "confirmation is on by default");
                assert!(!no_browser);
            }
            _ => panic!("wrong variant"),
        }
        assert!(matches!(
            parse(&["place", "+13175550142", "--yes"]).unwrap(),
            Cmd::Place {
                confirm: Confirm { yes: true, .. },
                ..
            }
        ));
        assert!(matches!(
            parse(&["place", "+13175550142", "--dry-run", "--contact-id", "abc", "--name", "Pat"]).unwrap(),
            Cmd::Place {
                confirm: Confirm { dry_run: true, .. },
                ..
            }
        ));
        assert!(parse(&["place"]).is_err(), "the number is required");
    }

    #[test]
    fn dial_e164_normalises_or_refuses() {
        assert_eq!(dial_e164("+13175550142").unwrap(), "+13175550142");
        assert_eq!(dial_e164("(317) 555-0142").unwrap(), "+13175550142");
        assert_eq!(dial_e164("1-317-555-0142").unwrap(), "+13175550142");
        assert_eq!(dial_e164(" +44 20 7946 0958 ").unwrap(), "+442079460958");
        assert!(dial_e164("555-0142").is_err(), "7 digits is a guess");
        assert!(dial_e164("+1234567").is_err(), "too short for E.164");
        assert!(dial_e164("+1234567890123456").is_err(), "too long for E.164");
        assert!(dial_e164("317555014x").is_err(), "letters refused");
        assert!(dial_e164("31755501+42").is_err(), "+ only first");
        assert!(dial_e164("23175550142").is_err(), "11 digits must start with 1");
    }

    #[test]
    fn path_ids_refuse_route_escapes() {
        assert_eq!(path_id("x", " 3f2a-b_9 ").unwrap(), "3f2a-b_9");
        for bad in ["", "a/b", "a?b", "a#b", "a b", "..", &"a".repeat(65)] {
            assert!(path_id("x", bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn filters_validate() {
        assert_eq!(direction("Inbound").unwrap(), "inbound");
        assert!(direction("sideways").is_err());
        assert_eq!(statuses("missed, Voicemail").unwrap(), "missed,voicemail");
        assert!(statuses("missed,bogus").is_err());
        assert!(statuses(" , ").is_err());
        assert_eq!(period("2026-09").unwrap(), "2026-09");
        for bad in ["2026-13", "2026-00", "2026-9", "26-09", "2026/09", "2026-09-01"] {
            assert!(period(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn list_path_matches_the_mcp_params() {
        assert_eq!(list_path("o1", &ListFilter::default()).unwrap(), "/organizations/o1/calling/calls?limit=25");
        let p = list_path(
            "o1",
            &ListFilter {
                direction: Some("outbound"),
                status: Some("missed,voicemail"),
                contact_id: Some("c-1"),
                since: Some("2026-09-01T00:00:00Z"),
                until: Some("2026-09-02T00:00:00+00:00"),
                mine: true,
                limit: Some(50),
                cursor: Some("abc=="),
            },
        )
        .unwrap();
        assert_eq!(
            p,
            "/organizations/o1/calling/calls?direction=outbound&status=missed%2Cvoicemail&contactId=c-1\
             &from=2026-09-01T00%3A00%3A00Z&to=2026-09-02T00%3A00%3A00%2B00%3A00&mine=true&limit=50&cursor=abc%3D%3D"
        );
        assert!(list_path(
            "o1",
            &ListFilter {
                contact_id: Some("../x"),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn voicemails_over_read_only_for_unheard() {
        assert_eq!(
            voicemails_path("o1", None, 10, false),
            "/organizations/o1/calling/calls?status=voicemail&limit=10"
        );
        assert_eq!(
            voicemails_path("o1", None, 10, true),
            "/organizations/o1/calling/calls?status=voicemail&limit=40"
        );
        assert_eq!(
            voicemails_path("o1", Some("2026-09-01T00:00:00Z"), 25, true),
            "/organizations/o1/calling/calls?status=voicemail&from=2026-09-01T00%3A00%3A00Z&limit=100"
        );
    }

    #[test]
    fn voicemail_filter_skips_heard_and_non_messages() {
        let unheard = json!({ "voicemailId": "v1", "voicemailHeardAt": null });
        let heard = json!({ "voicemailId": "v2", "voicemailHeardAt": "2026-09-01T00:00:00Z" });
        let none = json!({ "voicemailId": null });
        assert!(is_voicemail(&unheard, true));
        assert!(!is_voicemail(&heard, true));
        assert!(is_voicemail(&heard, false));
        assert!(!is_voicemail(&none, false));
        assert!(!is_voicemail(&json!({}), false));
    }

    #[test]
    fn usage_path_takes_an_optional_period() {
        assert_eq!(usage_path("o1", None).unwrap(), "/organizations/o1/calling/usage");
        assert_eq!(usage_path("o1", Some("2026-08")).unwrap(), "/organizations/o1/calling/usage?period=2026-08");
        assert!(usage_path("o1", Some("August")).is_err());
    }

    #[test]
    fn dial_link_matches_the_mcp() {
        assert_eq!(
            dial_link("https://smoo.ai/", "+13175550142", None),
            "https://smoo.ai/apps/phone?dial=%2B13175550142"
        );
        assert_eq!(
            dial_link("https://smoo.ai", "+13175550142", Some("c-1")),
            "https://smoo.ai/apps/phone?dial=%2B13175550142&contactId=c-1"
        );
    }

    #[test]
    fn can_dial_reports_reasons() {
        assert!(can_dial(&json!({ "canDial": true })).is_ok());
        assert_eq!(
            can_dial(&json!({ "canDial": false, "reasons": ["no_seat", "no_number"] })).unwrap_err(),
            "reasons: no_seat, no_number"
        );
        assert!(can_dial(&json!({})).unwrap_err().contains("not switched on"), "missing canDial fails closed");
    }
}
