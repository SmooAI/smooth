//! `th smoo org-notifications …` — the org's notification EMAIL recipients:
//! shared inboxes and members that receive instant emails and daily / weekly
//! digests about the org's bookings, deals, leads, escalations, missed calls
//! and conversations (SMOODEV-3828, smooai ADR-152). CLI twin of the copilot's
//! `org_notification_emails.*` tools and the hosted MCP
//! `org_notification_emails_*` tools.
//!
//! | verb       | route (under `/organizations/{org}/notification-emails`)      |
//! |------------|----------------------------------------------------------------|
//! | `list`     | `GET /recipients`, `GET /settings`                             |
//! | `add`      | `POST /recipients`                                             |
//! | `update`   | `PATCH /recipients/:id` and/or `PUT /recipients/:id/events`  |
//! | `remove`   | `DELETE /recipients/:id`                                      |
//! | `settings` | `GET /settings`, or `GET` + `PUT /settings` when flags are set |
//! | `test`     | `POST /recipients/:id/test`                                   |
//! | `resend`   | `POST /recipients/:id/resend-confirmation`                    |
//!
//! Not to be confused with `th smoo notifications`, which is YOUR OWN per-person
//! routing. This is org-level and needs `org.notifications.manage` for every
//! change (upstream enforces it; members can still `list`, with external
//! addresses masked). Every write goes through [`crate::destructive`]: it
//! prints the target, `--dry-run` stops there, and a script needs `--yes`.
//!
//! An external address is not sent anything until someone there opens the
//! confirmation link the server emails it; a member is active at once.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::{print_json, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// The event registry (mirrors smooai `OrgNotificationEventTypeValues` /
/// `rust/notify/src/org_events.rs`). Checked locally so a typo fails before the
/// round-trip; the server re-validates.
pub const EVENT_TYPES: &[&str] = &[
    "booking.created",
    "booking.rescheduled",
    "booking.cancelled",
    "deal.won",
    "deal.lost",
    "lead.captured",
    "escalation.created",
    "call.missed",
    "call.voicemail",
    "conversation.started",
];

/// Delivery modes.
pub const MODES: &[&str] = &["instant", "daily", "weekly"];

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's notification email recipients and the digest schedule.
    List {
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Add an address (e.g. a shared inbox) or a member as a recipient.
    Add {
        /// Any email address. Gets a confirmation email first.
        #[arg(long, conflicts_with = "member")]
        email: Option<String>,
        /// An org member's user id. Active immediately.
        #[arg(long)]
        member: Option<String>,
        /// Optional label, e.g. "Front desk inbox".
        #[arg(long)]
        name: Option<String>,
        /// `<event>:<mode>`, e.g. `deal.won:daily`. Repeatable. Events: booking.created,
        /// booking.rescheduled, booking.cancelled, deal.won, deal.lost, lead.captured,
        /// escalation.created, call.missed, call.voicemail, conversation.started.
        /// Modes: instant, daily, weekly.
        #[arg(long = "event")]
        events: Vec<String>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Relabel a recipient and/or REPLACE the events it receives.
    Update {
        /// Recipient id (from `list`).
        id: String,
        /// New label.
        #[arg(long)]
        name: Option<String>,
        /// `<event>:<mode>`; replaces the whole set. Repeatable.
        #[arg(long = "event")]
        events: Vec<String>,
        /// Stop every event (keeps the address configured).
        #[arg(long, conflicts_with = "events")]
        clear_events: bool,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Stop sending to a recipient.
    Remove {
        /// Recipient id (from `list`).
        id: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Show the digest schedule, or change it with any flag.
    Settings {
        /// IANA time zone, e.g. America/Chicago.
        #[arg(long)]
        tz: Option<String>,
        /// Local hour (0-23) the daily digest goes out.
        #[arg(long)]
        daily_hour: Option<i64>,
        /// Weekly digest day, 1 = Monday … 7 = Sunday.
        #[arg(long)]
        weekly_day: Option<i64>,
        /// Local hour (0-23) the weekly digest goes out.
        #[arg(long)]
        weekly_hour: Option<i64>,
        /// Quiet hours `HH:MM-HH:MM` (24h); instant emails wait until it ends.
        #[arg(long)]
        quiet: Option<String>,
        /// Remove quiet hours.
        #[arg(long, conflicts_with = "quiet")]
        no_quiet: bool,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Send one sample email to a confirmed recipient.
    Test {
        /// Recipient id (from `list`).
        id: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Email a pending recipient a fresh confirmation link.
    Resend {
        /// Recipient id (from `list`).
        id: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb, like the sibling modules
pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::List { json, org } => {
            let org = crate::active_org::resolve(org)?;
            let list = client.get(&base(&org, "/recipients")).await.context("GET notification-emails/recipients")?;
            let settings = client.get(&base(&org, "/settings")).await.context("GET notification-emails/settings")?;
            if json {
                print_json(&json!({ "recipients": list.get("recipients"), "canManage": list.get("canManage"), "settings": settings }));
            } else {
                println!();
                println!("{}", render_list(&list, &settings));
                println!();
            }
            Ok(())
        }
        Cmd::Add {
            email,
            member,
            name,
            events,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let body = add_body(email.as_deref(), member.as_deref(), name.as_deref(), &events)?;
            let who = email.or(member).unwrap_or_default();
            if !gate(&org, "add", "org notification email recipient", &who, confirm)? {
                return Ok(());
            }
            let r = client
                .post(&base(&org, "/recipients"), Some(&body))
                .await
                .context("POST notification-emails/recipients")?;
            println!("  {} added {}", "✚".green(), recipient_line(&r));
            if r.get("status").and_then(Value::as_str) == Some("pending") {
                println!(
                    "    {} a confirmation email is on its way; nothing is sent there until it is confirmed",
                    "→".dimmed()
                );
            }
            Ok(())
        }
        Cmd::Update {
            id,
            name,
            events,
            clear_events,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&id)?;
            let subs = if clear_events {
                Some(json!([]))
            } else if events.is_empty() {
                None
            } else {
                Some(parse_events(&events)?)
            };
            if name.is_none() && subs.is_none() {
                bail!("nothing to change — pass --name, --event or --clear-events");
            }
            if !gate(&org, "update", "org notification email recipient", &id, confirm)? {
                return Ok(());
            }
            let path = base(&org, &format!("/recipients/{id}"));
            let mut last = match name {
                Some(n) => client
                    .patch(&path, &json!({ "displayName": n }))
                    .await
                    .context("PATCH notification-emails/recipients/:id")?,
                None => Value::Null,
            };
            if let Some(s) = subs {
                last = client
                    .put(&format!("{path}/events"), &json!({ "subscriptions": s }))
                    .await
                    .context("PUT notification-emails/recipients/:id/events")?;
            }
            println!("  {} updated {}", "✓".green(), recipient_line(&last));
            Ok(())
        }
        Cmd::Remove { id, confirm, org } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&id)?;
            if !gate(&org, "remove", "org notification email recipient", &id, confirm)? {
                return Ok(());
            }
            client
                .delete(&base(&org, &format!("/recipients/{id}")))
                .await
                .context("DELETE notification-emails/recipients/:id")?;
            println!("  {} removed recipient {}", "🗑".red(), id.dimmed());
            Ok(())
        }
        Cmd::Settings {
            tz,
            daily_hour,
            weekly_day,
            weekly_hour,
            quiet,
            no_quiet,
            json,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let current = client.get(&base(&org, "/settings")).await.context("GET notification-emails/settings")?;
            let change = SettingsChange {
                tz,
                daily_hour,
                weekly_day,
                weekly_hour,
                quiet,
                no_quiet,
            };
            if change.is_empty() {
                if json {
                    print_json(&current);
                } else {
                    println!("  {}", settings_line(&current));
                }
                return Ok(());
            }
            let merged = merge_settings(&current, &change)?;
            if !gate(&org, "change", "org notification digest settings", &org, confirm)? {
                return Ok(());
            }
            let saved = client
                .put(&base(&org, "/settings"), &merged)
                .await
                .context("PUT notification-emails/settings")?;
            println!("  {} {}", "✓".green(), settings_line(&saved));
            Ok(())
        }
        Cmd::Test { id, confirm, org } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&id)?;
            if !gate(&org, "send a test email to", "org notification email recipient", &id, confirm)? {
                return Ok(());
            }
            client
                .post(&base(&org, &format!("/recipients/{id}/test")), None)
                .await
                .context("POST notification-emails/recipients/:id/test")?;
            println!("  {} sent a test email to recipient {}", "✉".green(), id.dimmed());
            Ok(())
        }
        Cmd::Resend { id, confirm, org } => {
            let org = crate::active_org::resolve(org)?;
            let id = check_id(&id)?;
            if !gate(&org, "resend the confirmation to", "org notification email recipient", &id, confirm)? {
                return Ok(());
            }
            let r = client
                .post(&base(&org, &format!("/recipients/{id}/resend-confirmation")), None)
                .await
                .context("POST notification-emails/recipients/:id/resend-confirmation")?;
            println!(
                "  {} sent a new confirmation link to {}",
                "✉".green(),
                r.get("email").and_then(Value::as_str).unwrap_or("the address")
            );
            Ok(())
        }
    }
}

fn gate(org: &str, verb: &str, noun: &str, id: &str, confirm: Confirm) -> Result<bool> {
    crate::destructive::gate_with(
        &Target {
            verb,
            noun,
            id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )
}

fn base(org: &str, suffix: &str) -> String {
    format!("/organizations/{org}/notification-emails{suffix}")
}

/// A recipient id for a URL path: hex digits and dashes only, so an argument can
/// never re-point the request at another route.
pub fn check_id(id: &str) -> Result<String> {
    let id = id.trim();
    if !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(id.to_string())
    } else {
        bail!("\"{id}\" is not a recipient id — see `th smoo org-notifications list`")
    }
}

/// `--event <event>:<mode>` flags → the `subscriptions` array.
pub fn parse_events(events: &[String]) -> Result<Value> {
    let mut out = Vec::new();
    for e in events {
        let Some((event, mode)) = e.trim().rsplit_once(':') else {
            bail!("--event \"{e}\" must be <event>:<mode>, e.g. deal.won:daily");
        };
        if !EVENT_TYPES.contains(&event) {
            bail!("unknown event \"{event}\" — use one of: {}", EVENT_TYPES.join(", "));
        }
        if !MODES.contains(&mode) {
            bail!("unknown mode \"{mode}\" — use one of: {}", MODES.join(", "));
        }
        let item = json!({ "eventType": event, "mode": mode });
        if !out.contains(&item) {
            out.push(item);
        }
    }
    Ok(Value::Array(out))
}

/// The `add` POST body.
pub fn add_body(email: Option<&str>, member: Option<&str>, name: Option<&str>, events: &[String]) -> Result<Value> {
    let mut body = Map::new();
    match (email.map(str::trim), member.map(str::trim)) {
        (Some(e), None) if !e.is_empty() => {
            if !e.contains('@') || e.contains(char::is_whitespace) || e.contains(',') {
                bail!("\"{e}\" is not one email address");
            }
            body.insert("email".into(), json!(e));
        }
        (None, Some(m)) if !m.is_empty() => {
            body.insert("userId".into(), json!(m));
        }
        _ => bail!("give exactly one of --email (any address, e.g. a shared inbox) or --member <user-id>"),
    }
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        body.insert("displayName".into(), json!(n));
    }
    body.insert("subscriptions".into(), parse_events(events)?);
    Ok(Value::Object(body))
}

/// The `settings` flags.
#[derive(Debug, Default)]
pub struct SettingsChange {
    pub tz: Option<String>,
    pub daily_hour: Option<i64>,
    pub weekly_day: Option<i64>,
    pub weekly_hour: Option<i64>,
    pub quiet: Option<String>,
    pub no_quiet: bool,
}

impl SettingsChange {
    fn is_empty(&self) -> bool {
        self.tz.is_none() && self.daily_hour.is_none() && self.weekly_day.is_none() && self.weekly_hour.is_none() && self.quiet.is_none() && !self.no_quiet
    }
}

fn is_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit)
        && s[..2].parse::<u8>().is_ok_and(|h| h < 24)
        && s[3..].parse::<u8>().is_ok_and(|m| m < 60)
}

/// Merge the flags onto the current settings (the PUT replaces the row).
pub fn merge_settings(current: &Value, c: &SettingsChange) -> Result<Value> {
    let mut s = current.as_object().cloned().unwrap_or_default();
    if let Some(tz) = c.tz.as_deref().map(str::trim) {
        if !tz.contains('/') && tz != "UTC" {
            bail!("--tz \"{tz}\" is not an IANA time zone (e.g. America/Chicago)");
        }
        s.insert("timeZone".into(), json!(tz));
    }
    for (flag, key, v, lo, hi) in [
        ("--daily-hour", "dailySendHour", c.daily_hour, 0, 23),
        ("--weekly-day", "weeklySendDay", c.weekly_day, 1, 7),
        ("--weekly-hour", "weeklySendHour", c.weekly_hour, 0, 23),
    ] {
        if let Some(n) = v {
            if !(lo..=hi).contains(&n) {
                bail!("{flag} must be {lo}-{hi}");
            }
            s.insert(key.into(), json!(n));
        }
    }
    if c.no_quiet {
        s.insert("quietStart".into(), Value::Null);
        s.insert("quietEnd".into(), Value::Null);
    } else if let Some(q) = c.quiet.as_deref() {
        let Some((a, b)) = q.trim().split_once('-') else {
            bail!("--quiet must be HH:MM-HH:MM, e.g. 21:00-07:00");
        };
        if !is_hhmm(a) || !is_hhmm(b) || a == b {
            bail!("--quiet must be two different 24h times, e.g. 21:00-07:00");
        }
        s.insert("quietStart".into(), json!(a));
        s.insert("quietEnd".into(), json!(b));
    }
    Ok(Value::Object(s))
}

fn recipient_line(r: &Value) -> String {
    let email = r.get("email").and_then(Value::as_str).unwrap_or("?");
    let who = match r.get("displayName").and_then(Value::as_str).filter(|n| !n.is_empty()) {
        Some(n) => format!("{n} <{email}>"),
        None => email.to_string(),
    };
    let status = r.get("status").and_then(Value::as_str).unwrap_or("?");
    let subs: Vec<String> = r
        .get("subscriptions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|s| {
                    format!(
                        "{}:{}",
                        s.get("eventType").and_then(Value::as_str).unwrap_or("?"),
                        s.get("mode").and_then(Value::as_str).unwrap_or("?")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "{who} [{status}] {} — {}",
        r.get("id").and_then(Value::as_str).unwrap_or("?"),
        if subs.is_empty() { "no events".to_string() } else { subs.join(", ") }
    )
}

fn settings_line(s: &Value) -> String {
    let n = |k: &str| s.get(k).and_then(Value::as_i64).unwrap_or(0);
    let day = ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        .get(usize::try_from(n("weeklySendDay")).unwrap_or(0))
        .copied()
        .unwrap_or("?");
    let quiet = match (s.get("quietStart").and_then(Value::as_str), s.get("quietEnd").and_then(Value::as_str)) {
        (Some(a), Some(b)) => format!("quiet {a}-{b}"),
        _ => "no quiet hours".to_string(),
    };
    format!(
        "{} · daily digest {:02}:00 · weekly digest {day} {:02}:00 · {quiet}",
        s.get("timeZone").and_then(Value::as_str).unwrap_or("UTC"),
        n("dailySendHour"),
        n("weeklySendHour")
    )
}

fn render_list(list: &Value, settings: &Value) -> String {
    let rows: Vec<String> = list
        .get("recipients")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|r| format!("  {}", recipient_line(r))).collect())
        .unwrap_or_default();
    let head = if rows.is_empty() {
        "  No notification email recipients yet — `th smoo org-notifications add`.".to_string()
    } else {
        rows.join("\n")
    };
    format!("{head}\n\n  {}", settings_line(settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn events_parse_dedupe_and_refuse_unknowns() {
        assert_eq!(
            parse_events(&s(&["deal.won:daily", "booking.created:instant", "deal.won:daily"])).unwrap(),
            json!([{ "eventType": "deal.won", "mode": "daily" }, { "eventType": "booking.created", "mode": "instant" }])
        );
        assert!(parse_events(&s(&["deal.won"])).is_err());
        assert!(parse_events(&s(&["deal.closed:daily"])).is_err());
        assert!(parse_events(&s(&["deal.won:hourly"])).is_err());
        assert_eq!(parse_events(&[]).unwrap(), json!([]));
    }

    #[test]
    fn add_body_needs_exactly_one_target() {
        assert!(add_body(None, None, None, &[]).is_err());
        assert!(add_body(Some("a@b.com"), Some("u"), None, &[]).is_err());
        assert!(add_body(Some("a@b.com,c@d.com"), None, None, &[]).is_err());
        assert_eq!(
            add_body(Some(" team@x.com "), None, Some("Ops"), &s(&["lead.captured:weekly"])).unwrap(),
            json!({ "email": "team@x.com", "displayName": "Ops", "subscriptions": [{ "eventType": "lead.captured", "mode": "weekly" }] })
        );
        assert_eq!(add_body(None, Some("u-1"), None, &[]).unwrap(), json!({ "userId": "u-1", "subscriptions": [] }));
    }

    #[test]
    fn ids_cannot_repoint_the_route() {
        assert!(check_id("../settings").is_err());
        assert!(check_id("abc/test").is_err());
        assert!(check_id("").is_err());
        assert_eq!(
            check_id(" 11111111-2222-3333-4444-555555555555 ").unwrap(),
            "11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn settings_merge_validates_and_keeps_the_rest() {
        let cur = json!({ "timeZone": "UTC", "dailySendHour": 17, "weeklySendDay": 1, "weeklySendHour": 8, "quietStart": null, "quietEnd": null });
        let m = merge_settings(
            &cur,
            &SettingsChange {
                tz: Some("America/Chicago".into()),
                quiet: Some("21:00-07:00".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m["timeZone"], "America/Chicago");
        assert_eq!(m["dailySendHour"], 17);
        assert_eq!((m["quietStart"].as_str(), m["quietEnd"].as_str()), (Some("21:00"), Some("07:00")));
        let cleared = merge_settings(
            &m,
            &SettingsChange {
                no_quiet: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(cleared["quietStart"].is_null());
        for bad in [
            SettingsChange {
                tz: Some("Central".into()),
                ..Default::default()
            },
            SettingsChange {
                daily_hour: Some(24),
                ..Default::default()
            },
            SettingsChange {
                weekly_day: Some(0),
                ..Default::default()
            },
            SettingsChange {
                quiet: Some("21:00".into()),
                ..Default::default()
            },
            SettingsChange {
                quiet: Some("21:00-21:00".into()),
                ..Default::default()
            },
            SettingsChange {
                quiet: Some("9:00-07:00".into()),
                ..Default::default()
            },
        ] {
            assert!(merge_settings(&cur, &bad).is_err(), "{bad:?}");
        }
        assert!(SettingsChange::default().is_empty());
    }

    #[test]
    fn rendering_shows_status_events_and_schedule() {
        let list = json!({ "recipients": [{ "id": "r1", "email": "team@x.com", "displayName": "Team", "status": "pending",
            "subscriptions": [{ "eventType": "deal.won", "mode": "daily" }] }] });
        let out = render_list(
            &list,
            &json!({ "timeZone": "America/Chicago", "dailySendHour": 7, "weeklySendDay": 1, "weeklySendHour": 8 }),
        );
        assert!(out.contains("Team <team@x.com> [pending] r1 — deal.won:daily"), "{out}");
        assert!(
            out.contains("America/Chicago · daily digest 07:00 · weekly digest Mon 08:00 · no quiet hours"),
            "{out}"
        );
        assert!(render_list(&json!({ "recipients": [] }), &json!({})).contains("No notification email recipients yet"));
    }

    #[test]
    fn paths_stay_under_the_org() {
        assert_eq!(base("org-1", "/recipients"), "/organizations/org-1/notification-emails/recipients");
    }
}
