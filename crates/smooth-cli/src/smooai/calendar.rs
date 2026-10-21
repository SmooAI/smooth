//! `th smoo calendar …` — YOUR connected calendars (Google, Microsoft 365,
//! iCloud): list them, read and write events, answer invitations, find open
//! time and block it (SMOODEV-3764, ADR-147 §2 — "the MCP tools, then the CLI").
//!
//! CLI twin of the hosted MCP `calendar_*` tools, over the same routes:
//!
//! | Verb                       | MCP tool                  | Route                                              |
//! | -------------------------- | ------------------------- | -------------------------------------------------- |
//! | `list`                     | `calendar_list_calendars` | `GET    /organizations/{org}/calendars`            |
//! | `events list`              | `calendar_list_events`    | `GET    /organizations/{org}/calendars/events`     |
//! | `events show`              | `calendar_get_event`      | `GET    /organizations/{org}/calendars/event`      |
//! | `events create`            | `calendar_create_event`   | `POST   /organizations/{org}/calendars/events`     |
//! | `events update`            | `calendar_update_event`   | `PATCH  /organizations/{org}/calendars/event`      |
//! | `events rm`                | `calendar_delete_event`   | `DELETE /organizations/{org}/calendars/event`      |
//! | `events respond`           | `calendar_respond`        | `POST   /organizations/{org}/calendars/event/respond` |
//! | `find-time`                | `calendar_find_time`      | `POST   /organizations/{org}/calendars/find-time`  |
//! | `block`                    | `calendar_create_block`   | `POST   /organizations/{org}/calendars/blocks`     |
//!
//! **Whose calendar (ADR-147 §1).** Every route runs as the signed-in person:
//! their own calendars, org calendars, and ones a teammate shared with them.
//! An org API key has no calendar, so this needs `smoo auth login`.
//!
//! A calendar is named by the opaque `calendarRef` that `list` prints, and an
//! event by its id; both travel in the QUERY, because Microsoft Graph event ids
//! carry `/`, `+` and `=` and iCloud calendar ids are URLs.
//!
//! **Confirmation.** `events rm` (attendees get a cancellation), `events
//! respond` (the organizer is told), `events create` with attendees (each gets
//! a REAL invitation) and `events update` on an event with guests (they are
//! notified) all go through the shared [`crate::destructive`] gate: the target
//! is printed first, `--dry-run` stops there, a terminal is asked, and a script
//! must pass `--yes`. `block` and an attendee-less `create` reach nobody, so
//! they run straight away.

use anstream::{eprintln, println};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset, Local, TimeZone};
use chrono_tz::Tz;
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::{print_json, require_active_org, require_user_session};

/// Most attendees one event may carry (`eventInputSchema`'s `max(100)`).
const MAX_ATTENDEES: usize = 100;
/// Most calendars one events / find-time request may name.
const MAX_CALENDARS: usize = 20;
/// Most attendees find-time checks free/busy for.
const MAX_FIND_TIME_ATTENDEES: usize = 20;
/// Events shown by `events list` unless `--limit` says otherwise (the MCP's default).
const DEFAULT_EVENT_LIMIT: usize = 25;

#[derive(Subcommand)]
pub enum Cmd {
    /// List every calendar you can use, with the calendarRef each verb takes.
    ///
    /// Your own calendars, organization calendars, and ones a teammate shared
    /// with you. `●` can write, `○` is read-only.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Events on your calendars — list, show, create, update, rm, respond.
    #[command(visible_alias = "event")]
    Events {
        #[command(subcommand)]
        cmd: EventsCmd,
    },
    /// Find open meeting slots across your calendars (and, best effort, attendees').
    ///
    /// Working hours are `--workday-start`–`--workday-end` in `--time-zone`
    /// (default 09:00–17:00, Monday–Friday). Anyone whose free/busy could not
    /// be checked is reported as unchecked, never as free.
    #[command(name = "find-time")]
    FindTime {
        /// Meeting length in minutes (5–480).
        #[arg(long, value_parser = clap::value_parser!(u32).range(5..=480))]
        duration: u32,
        /// IANA time zone the working hours are in (e.g. America/New_York). Default: this machine's.
        #[arg(long = "time-zone", visible_alias = "tz")]
        time_zone: Option<String>,
        /// Search from, RFC3339 with an offset. Default: now.
        #[arg(long)]
        from: Option<String>,
        /// Search until, RFC3339 with an offset. Default: 7 days after --from; at most 62 days.
        #[arg(long)]
        to: Option<String>,
        /// calendarRefs to treat as busy (repeat or comma-separate). Default: every calendar on your own accounts.
        #[arg(long = "calendar", value_delimiter = ',')]
        calendars: Vec<String>,
        /// Other people whose free/busy to check too (repeat or comma-separate).
        #[arg(long = "attendee", value_delimiter = ',')]
        attendees: Vec<String>,
        /// Working day start, HH:MM (24h). Default 09:00.
        #[arg(long = "workday-start")]
        workday_start: Option<String>,
        /// Working day end, HH:MM (24h). Default 17:00.
        #[arg(long = "workday-end")]
        workday_end: Option<String>,
        /// Weekdays to search, comma-separated, 0 = Sunday … 6 = Saturday. Default 1,2,3,4,5.
        #[arg(long, value_delimiter = ',')]
        days: Vec<u8>,
        /// Most slots to return (1–50, default 10).
        #[arg(long = "max-slots", value_parser = clap::value_parser!(u32).range(1..=50))]
        max_slots: Option<u32>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the slots.
        #[arg(long)]
        json: bool,
    },
    /// Block time on a calendar: a busy event with no guests ("Focus time").
    Block {
        /// Start, RFC3339 with an offset (e.g. 2026-10-21T13:00:00-04:00).
        #[arg(long)]
        start: String,
        /// End, RFC3339 with an offset, after --start.
        #[arg(long)]
        end: String,
        /// Title shown on the calendar. Default "Focus time".
        #[arg(long)]
        title: Option<String>,
        /// IANA time zone the block is meant in. Default: the calendar's.
        #[arg(long = "time-zone", visible_alias = "tz")]
        time_zone: Option<String>,
        /// calendarRef from `smoo calendar list`. Default: your own default calendar, when there is exactly one.
        #[arg(long)]
        calendar: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
    },
}

/// The fields `events create` and `events update` share. Every one is
/// optional here; `create` checks the required ones itself.
#[derive(clap::Args, Debug, Default, Clone)]
pub struct EventFields {
    /// Event title.
    #[arg(long)]
    pub title: Option<String>,
    /// Start, RFC3339 with an offset (e.g. 2026-10-21T15:00:00-04:00).
    #[arg(long)]
    pub start: Option<String>,
    /// End, RFC3339 with an offset, after --start.
    #[arg(long)]
    pub end: Option<String>,
    /// IANA time zone the event is meant in (e.g. America/New_York). Default: the calendar's.
    #[arg(long = "time-zone", visible_alias = "tz")]
    pub time_zone: Option<String>,
    /// An all-day event (start and end at midnight; end is exclusive).
    #[arg(long = "all-day")]
    pub all_day: bool,
    /// Location.
    #[arg(long)]
    pub location: Option<String>,
    /// Agenda / description.
    #[arg(long)]
    pub description: Option<String>,
    /// Attach the provider's meeting link (Google Meet / Teams). Not on iCloud.
    #[arg(long)]
    pub conference: bool,
    /// Mark the time free instead of busy.
    #[arg(long)]
    pub free: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum EventsCmd {
    /// List events, merged across calendars and sorted by start.
    ///
    /// Default: the main calendar of every account you can use, over the next
    /// 7 days (a window can span at most 62).
    List {
        /// Window start, RFC3339 (e.g. 2026-10-21T00:00:00Z). Default: now.
        #[arg(long)]
        from: Option<String>,
        /// Window end, RFC3339. Default: 7 days after --from.
        #[arg(long)]
        to: Option<String>,
        /// calendarRefs from `smoo calendar list` (repeat or comma-separate, at most 20).
        #[arg(long = "calendar", value_delimiter = ',')]
        calendars: Vec<String>,
        /// Free-text filter, where the provider supports one.
        #[arg(long)]
        query: Option<String>,
        /// Most events shown (default 25). `--json` always prints everything.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=500))]
        limit: Option<u32>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the list.
        #[arg(long)]
        json: bool,
    },
    /// Show one event in full: attendees, description, meeting link.
    #[command(visible_alias = "get")]
    Show {
        /// The event's calendarRef, as `events list` printed it.
        #[arg(long)]
        calendar: String,
        /// The event id, as `events list` printed it.
        #[arg(long = "event")]
        event: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the event.
        #[arg(long)]
        json: bool,
    },
    /// Create an event. Every --attendee receives a REAL invitation, so it confirms first.
    Create {
        #[command(flatten)]
        fields: EventFields,
        /// Attendee email (repeat or comma-separate). Each one gets a real invitation.
        #[arg(long = "attendee", value_delimiter = ',')]
        attendees: Vec<String>,
        /// calendarRef from `smoo calendar list`. Default: your own default calendar, when there is exactly one.
        #[arg(long)]
        calendar: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
    /// Change an event. Only the flags you pass change; --attendee REPLACES the guest list.
    ///
    /// Confirms first when the event has guests (they are notified) or the
    /// guest list changes.
    Update {
        /// The event's calendarRef, as `events list` printed it.
        #[arg(long)]
        calendar: String,
        /// The event id, as `events list` printed it.
        #[arg(long = "event")]
        event: String,
        #[command(flatten)]
        fields: EventFields,
        /// Make it a timed event again (the opposite of --all-day).
        #[arg(long = "not-all-day", conflicts_with = "all_day")]
        not_all_day: bool,
        /// Mark the time busy again (the opposite of --free).
        #[arg(long, conflicts_with = "free")]
        busy: bool,
        /// Drop the meeting link (the opposite of --conference).
        #[arg(long = "no-conference", conflicts_with = "conference")]
        no_conference: bool,
        /// New guest list (repeat or comma-separate) — REPLACES the current one.
        #[arg(long = "attendee", value_delimiter = ',')]
        attendees: Vec<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
    /// Delete an event. Its attendees get a cancellation; confirms first.
    #[command(visible_alias = "delete")]
    Rm {
        /// The event's calendarRef, as `events list` printed it.
        #[arg(long)]
        calendar: String,
        /// The event id, as `events list` printed it.
        #[arg(long = "event")]
        event: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
    /// Answer an invitation (accepted / declined / tentative). The organizer is told; confirms first.
    ///
    /// Google and Microsoft 365 only; iCloud calendars can't answer here.
    #[command(visible_alias = "rsvp")]
    Respond {
        /// The event's calendarRef, as `events list` printed it.
        #[arg(long)]
        calendar: String,
        /// The event id, as `events list` printed it.
        #[arg(long = "event")]
        event: String,
        /// Your answer.
        #[arg(long, value_parser = ["accepted", "declined", "tentative"])]
        response: String,
        /// Optional note to the organizer.
        #[arg(long)]
        comment: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
}

// ── Request building (pure) ──────────────────────────────────────────────────

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// An instant the API accepts: RFC3339 with `Z` or an offset (zod's
/// `datetime({ offset: true })`). Checked here so a typo fails before login.
fn instant(label: &str, s: &str) -> Result<String> {
    let s = s.trim();
    DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("--{label} must be RFC3339 with an offset, e.g. 2026-10-21T15:00:00-04:00 or 2026-10-21T19:00:00Z; got {s:?}"))?;
    Ok(s.to_string())
}

/// `end` after `start`, when both are given (the route's `assertOrder`).
fn check_order(start: Option<&str>, end: Option<&str>) -> Result<()> {
    if let (Some(s), Some(e)) = (start, end) {
        if DateTime::parse_from_rfc3339(e)? <= DateTime::parse_from_rfc3339(s)? {
            bail!("--end must be after --start");
        }
    }
    Ok(())
}

/// Attendees are invited by email: drop blanks, refuse anything not shaped
/// like an address (each one would get a real invitation), cap the count.
fn attendee_emails(raw: &[String], max: usize) -> Result<Vec<String>> {
    let list: Vec<String> = raw.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    if list.len() > max {
        bail!("at most {max} attendees per request; got {}", list.len());
    }
    if let Some(bad) = list.iter().find(|a| {
        let Some((local, domain)) = a.split_once('@') else { return true };
        local.is_empty() || !domain.contains('.') || a.chars().any(char::is_whitespace)
    }) {
        bail!("{bad:?} is not an email address — attendees are invited by email");
    }
    Ok(list)
}

/// calendarRefs for a query or body: trimmed, non-empty, at most 20.
fn calendar_refs(raw: &[String]) -> Result<Vec<String>> {
    let refs: Vec<String> = raw.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    if refs.len() > MAX_CALENDARS {
        bail!("at most {MAX_CALENDARS} calendars per request; got {}", refs.len());
    }
    Ok(refs)
}

fn required<'a>(label: &str, v: Option<&'a str>) -> Result<&'a str> {
    non_blank(v).with_context(|| format!("--{label} is required"))
}

/// `GET /calendars/events?from&to&q&calendarRefs` — only what was passed.
fn events_path(org: &str, from: Option<&str>, to: Option<&str>, calendars: &[String], query: Option<&str>) -> Result<String> {
    let mut params: Vec<String> = Vec::new();
    if let Some(f) = non_blank(from) {
        params.push(format!("from={}", urlencoding::encode(&instant("from", f)?)));
    }
    if let Some(t) = non_blank(to) {
        params.push(format!("to={}", urlencoding::encode(&instant("to", t)?)));
    }
    check_order(non_blank(from), non_blank(to)).context("--to must be after --from")?;
    if let Some(q) = non_blank(query) {
        params.push(format!("q={}", urlencoding::encode(q)));
    }
    let refs = calendar_refs(calendars)?;
    if !refs.is_empty() {
        params.push(format!("calendarRefs={}", urlencoding::encode(&refs.join(","))));
    }
    let mut path = format!("/organizations/{org}/calendars/events");
    if !params.is_empty() {
        path.push('?');
        path.push_str(&params.join("&"));
    }
    Ok(path)
}

/// `/calendars/event?calendarRef&eventId` — the single-event door (GET, PATCH, DELETE).
fn event_path(org: &str, calendar: &str, event: &str) -> Result<String> {
    let calendar = required("calendar", Some(calendar))?;
    let event = required("event", Some(event))?;
    Ok(format!(
        "/organizations/{org}/calendars/event?calendarRef={}&eventId={}",
        urlencoding::encode(calendar),
        urlencoding::encode(event)
    ))
}

/// The `POST /calendars/events` body (without `calendarRef`, which may be
/// resolved later). Title, start and end are required.
fn create_body(f: &EventFields, attendees: &[String]) -> Result<Map<String, Value>> {
    let title = required("title", f.title.as_deref())?;
    let start = instant("start", required("start", f.start.as_deref())?)?;
    let end = instant("end", required("end", f.end.as_deref())?)?;
    check_order(Some(&start), Some(&end))?;
    let mut b = Map::new();
    b.insert("title".into(), json!(title));
    b.insert("start".into(), json!(start));
    b.insert("end".into(), json!(end));
    if let Some(v) = non_blank(f.time_zone.as_deref()) {
        b.insert("timeZone".into(), json!(v));
    }
    if f.all_day {
        b.insert("allDay".into(), json!(true));
    }
    if let Some(v) = non_blank(f.location.as_deref()) {
        b.insert("location".into(), json!(v));
    }
    if let Some(v) = non_blank(f.description.as_deref()) {
        b.insert("description".into(), json!(v));
    }
    if f.conference {
        b.insert("conference".into(), json!(true));
    }
    if f.free {
        b.insert("free".into(), json!(true));
    }
    let attendees = attendee_emails(attendees, MAX_ATTENDEES)?;
    if !attendees.is_empty() {
        b.insert("attendees".into(), json!(attendees.iter().map(|a| json!({ "email": a })).collect::<Vec<_>>()));
    }
    Ok(b)
}

/// The boolean switches `events update` can flip either way.
#[derive(Debug, Default, Clone, Copy)]
struct Toggles {
    not_all_day: bool,
    busy: bool,
    no_conference: bool,
}

/// The `PATCH /calendars/event` body: a true patch, so only what was passed.
/// An empty patch is refused rather than sent as a no-op.
fn patch_body(f: &EventFields, t: Toggles, attendees: &[String]) -> Result<Value> {
    let mut b = Map::new();
    if let Some(v) = non_blank(f.title.as_deref()) {
        b.insert("title".into(), json!(v));
    }
    let start = non_blank(f.start.as_deref()).map(|s| instant("start", s)).transpose()?;
    let end = non_blank(f.end.as_deref()).map(|s| instant("end", s)).transpose()?;
    check_order(start.as_deref(), end.as_deref())?;
    if let Some(v) = start {
        b.insert("start".into(), json!(v));
    }
    if let Some(v) = end {
        b.insert("end".into(), json!(v));
    }
    if let Some(v) = non_blank(f.time_zone.as_deref()) {
        b.insert("timeZone".into(), json!(v));
    }
    if f.all_day || t.not_all_day {
        b.insert("allDay".into(), json!(f.all_day));
    }
    // Location and description are sent as given: `--location ""` clears one.
    if let Some(v) = &f.location {
        b.insert("location".into(), json!(v.trim()));
    }
    if let Some(v) = &f.description {
        b.insert("description".into(), json!(v.trim()));
    }
    if f.conference || t.no_conference {
        b.insert("conference".into(), json!(f.conference));
    }
    if f.free || t.busy {
        b.insert("free".into(), json!(f.free));
    }
    let attendees = attendee_emails(attendees, MAX_ATTENDEES)?;
    if !attendees.is_empty() {
        b.insert("attendees".into(), json!(attendees.iter().map(|a| json!({ "email": a })).collect::<Vec<_>>()));
    }
    if b.is_empty() {
        bail!("nothing to change — pass at least one of --title, --start, --end, --time-zone, --location, --description, --attendee, --all-day/--not-all-day, --conference/--no-conference, --free/--busy");
    }
    Ok(Value::Object(b))
}

/// The `POST /calendars/blocks` body (without `calendarRef`).
fn block_body(start: &str, end: &str, title: Option<&str>, time_zone: Option<&str>) -> Result<Map<String, Value>> {
    let start = instant("start", start)?;
    let end = instant("end", end)?;
    check_order(Some(&start), Some(&end))?;
    let mut b = Map::new();
    b.insert("start".into(), json!(start));
    b.insert("end".into(), json!(end));
    if let Some(v) = non_blank(title) {
        b.insert("title".into(), json!(v));
    }
    if let Some(v) = non_blank(time_zone) {
        b.insert("timeZone".into(), json!(v));
    }
    Ok(b)
}

/// The `POST /calendars/event/respond` body.
fn respond_body(calendar: &str, event: &str, response: &str, comment: Option<&str>) -> Result<Value> {
    if !matches!(response, "accepted" | "declined" | "tentative") {
        bail!("--response must be accepted, declined or tentative; got {response:?}");
    }
    let mut b = json!({
        "calendarRef": required("calendar", Some(calendar))?,
        "eventId": required("event", Some(event))?,
        "response": response,
    });
    if let Some(c) = non_blank(comment) {
        b["comment"] = json!(c);
    }
    Ok(b)
}

/// "HH:MM" → minutes from midnight; `24:00` is allowed as an end of day.
fn hhmm(label: &str, s: &str) -> Result<u32> {
    let bad = || format!("--{label} must be HH:MM (24h), e.g. 09:00; got {s:?}");
    let (h, m) = s.trim().split_once(':').with_context(bad)?;
    let (h, m): (u32, u32) = (h.parse().with_context(bad)?, m.parse().with_context(bad)?);
    if m > 59 || h > 24 || (h == 24 && m != 0) {
        bail!(bad());
    }
    Ok(h * 60 + m)
}

/// Resolved `find-time` flags, kept apart from clap so the body is testable.
struct FindTime<'a> {
    duration: u32,
    time_zone: &'a str,
    from: Option<&'a str>,
    to: Option<&'a str>,
    calendars: &'a [String],
    attendees: &'a [String],
    workday_start: Option<&'a str>,
    workday_end: Option<&'a str>,
    days: &'a [u8],
    max_slots: Option<u32>,
}

/// The `POST /calendars/find-time` body. Omitted options are left to the
/// route's defaults (09:00–17:00, Monday–Friday, 10 slots).
fn find_time_body(a: &FindTime) -> Result<Value> {
    a.time_zone
        .parse::<Tz>()
        .map_err(|_| anyhow::anyhow!("--time-zone must be an IANA zone like America/New_York; got {:?}", a.time_zone))?;
    let mut b = Map::new();
    b.insert("durationMinutes".into(), json!(a.duration));
    b.insert("timeZone".into(), json!(a.time_zone));
    if let Some(f) = non_blank(a.from) {
        b.insert("from".into(), json!(instant("from", f)?));
    }
    if let Some(t) = non_blank(a.to) {
        b.insert("to".into(), json!(instant("to", t)?));
    }
    check_order(non_blank(a.from), non_blank(a.to)).context("--to must be after --from")?;
    let refs = calendar_refs(a.calendars)?;
    if !refs.is_empty() {
        b.insert("calendarRefs".into(), json!(refs));
    }
    let attendees = attendee_emails(a.attendees, MAX_FIND_TIME_ATTENDEES)?;
    if !attendees.is_empty() {
        b.insert("attendeeEmails".into(), json!(attendees));
    }
    let start = non_blank(a.workday_start).map(|s| hhmm("workday-start", s)).transpose()?;
    let end = non_blank(a.workday_end).map(|s| hhmm("workday-end", s)).transpose()?;
    if start.unwrap_or(9 * 60) >= end.unwrap_or(17 * 60) {
        bail!("--workday-end must be after --workday-start");
    }
    if let Some(s) = start {
        b.insert("workdayStartMinute".into(), json!(s));
    }
    if let Some(e) = end {
        b.insert("workdayEndMinute".into(), json!(e));
    }
    if !a.days.is_empty() {
        if let Some(bad) = a.days.iter().find(|d| **d > 6) {
            bail!("--days are 0 = Sunday … 6 = Saturday; got {bad}");
        }
        let mut days = a.days.to_vec();
        days.sort_unstable();
        days.dedup();
        b.insert("days".into(), json!(days));
    }
    if let Some(n) = a.max_slots {
        b.insert("maxSlots".into(), json!(n));
    }
    Ok(Value::Object(b))
}

/// This machine's IANA zone from the `/etc/localtime` symlink
/// (`…/zoneinfo/America/New_York`), when it resolves to a real zone.
fn zone_from_localtime_target(target: &str) -> Option<String> {
    let (_, name) = target.split_once("zoneinfo/")?;
    name.parse::<Tz>().ok().map(|_| name.to_string())
}

fn local_zone() -> Option<String> {
    if let Some(tz) = std::env::var("TZ").ok().filter(|t| t.parse::<Tz>().is_ok()) {
        return Some(tz);
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    zone_from_localtime_target(&target.to_string_lossy())
}

/// The calendar a create/block writes to when `--calendar` is omitted: the
/// caller's OWN default calendar that can write, when there is exactly one.
/// Two (a Google and a Microsoft account, say) is a question, not a guess.
fn pick_default_calendar(body: &Value) -> Result<(String, String)> {
    let calendars = body.get("calendars").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    if calendars.is_empty() {
        bail!(
            "you have no calendar connected, so nothing was written. Connect one with \
             `smoo integrations connect google --purpose features --tier full_access` (or `microsoft`), then retry."
        );
    }
    let usable = |c: &&Value| {
        c.get("own").and_then(Value::as_bool) == Some(true)
            && c.get("isDefault").and_then(Value::as_bool) == Some(true)
            && c.get("canWrite").and_then(Value::as_bool) == Some(true)
            && c.get("calendarRef").and_then(Value::as_str).is_some()
    };
    let picks: Vec<&Value> = calendars.iter().filter(usable).collect();
    match picks.as_slice() {
        [one] => Ok((s(one, "calendarRef").to_string(), calendar_label(one))),
        [] => bail!("none of your own default calendars can be written to — pass --calendar <ref> (see `smoo calendar list`)"),
        many => bail!(
            "you have {} default calendars ({}) — pass --calendar <ref> to pick one (see `smoo calendar list`)",
            many.len(),
            many.iter().map(|c| calendar_label(c)).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Guests other than the calendar's own account — the people a change reaches.
fn guests(event: &Value) -> Vec<String> {
    event
        .get("attendees")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|p| p.get("self").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("email").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// "3 attendees (a@x.com, b@x.com, +1 more)" — the banner's guest summary.
fn guest_summary(emails: &[String]) -> String {
    let shown: Vec<&str> = emails.iter().take(3).map(String::as_str).collect();
    let more = emails.len().saturating_sub(shown.len());
    let tail = if more > 0 { format!(", +{more} more") } else { String::new() };
    let noun = if emails.len() == 1 { "attendee" } else { "attendees" };
    format!("{} {noun} ({}{tail})", emails.len(), shown.join(", "))
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// A 409 means no usable calendar; a 404 means the calendar or event is not
/// one you can reach. Both name the next step; anything else keeps its context.
fn calendar_error(err: anyhow::Error, what: &str) -> anyhow::Error {
    let msg = format!("{err:#}");
    if msg.contains("HTTP 409") {
        anyhow::anyhow!(
            "no usable calendar is connected for you, so nothing was done. Connect one with \
             `smoo integrations connect google --purpose features --tier full_access` (or `microsoft`), then retry. ({msg})"
        )
    } else if msg.contains("HTTP 404") {
        anyhow::anyhow!(
            "{what}: not found — the calendar is not one you can use, or the event id is wrong. \
             Check `smoo calendar list` and `smoo calendar events list`. ({msg})"
        )
    } else {
        err.context(what.to_string())
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn calendar_label(c: &Value) -> String {
    let name = if s(c, "name").is_empty() { "Calendar" } else { s(c, "name") };
    match s(c, "accountEmail") {
        "" => format!("{name} [{}]", s(c, "provider")),
        e if e == name => format!("{name} [{}]", s(c, "provider")),
        e => format!("{name} <{e}> [{}]", s(c, "provider")),
    }
}

/// "own", "org", or "shared" — whose calendar it is.
fn calendar_owner(c: &Value) -> &'static str {
    if c.get("own").and_then(Value::as_bool) == Some(true) {
        "own"
    } else if s(c, "scope") == "org" {
        "org"
    } else {
        "shared"
    }
}

fn render_calendars(body: &Value) {
    let calendars = body.get("calendars").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    println!();
    if calendars.is_empty() {
        println!(
            "  {} no calendars — none of yours is connected and none is shared with you. This is a confirmed read, not a failure.",
            "●".dimmed()
        );
        println!(
            "  {} connect one: smoo integrations connect google --purpose features --tier full_access",
            "→".cyan()
        );
        println!();
        return;
    }
    for c in calendars {
        let errored = s(c, "status") == "error";
        let writable = c.get("canWrite").and_then(Value::as_bool) == Some(true);
        let glyph = if errored {
            "!".yellow().to_string()
        } else if writable {
            "●".green().to_string()
        } else {
            "○".dimmed().to_string()
        };
        let mut tags = vec![calendar_owner(c).to_string()];
        if c.get("isDefault").and_then(Value::as_bool) == Some(true) {
            tags.push("default".into());
        }
        tags.push(if writable { "read-write".into() } else { "read-only".into() });
        if let Some(tz) = c.get("timeZone").and_then(Value::as_str) {
            tags.push(tz.to_string());
        }
        println!("  {glyph} {}  {}", calendar_label(c).bold(), tags.join(" · ").dimmed());
        if errored {
            println!("      {}", "could not be read right now — the connection may need reconnecting".yellow());
        } else {
            println!("      {} {}", "calendar".dimmed(), s(c, "calendarRef").cyan());
        }
    }
    println!();
}

/// "Tue Oct 21 15:00–15:30" in `tz`; all-day events show dates only, and a
/// range crossing midnight shows both days.
fn fmt_range<Z: TimeZone>(start: &str, end: &str, all_day: bool, tz: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let (Ok(st), Ok(en)) = (DateTime::parse_from_rfc3339(start), DateTime::parse_from_rfc3339(end)) else {
        return format!("{start} – {end}");
    };
    if all_day {
        // All-day instants are midnight-to-midnight in the event's own zone and
        // `end` is exclusive: show the dates as the event states them.
        let last = en - chrono::Duration::days(1);
        let (a, b) = (st.date_naive(), last.date_naive());
        return if b <= a {
            format!("{} (all day)", a.format("%a %b %-d"))
        } else {
            format!("{} – {} (all day)", a.format("%a %b %-d"), b.format("%a %b %-d"))
        };
    }
    let (st, en) = (st.with_timezone(tz), en.with_timezone(tz));
    if st.date_naive() == en.date_naive() {
        format!("{}–{}", st.format("%a %b %-d %H:%M"), en.format("%H:%M"))
    } else {
        format!("{} – {}", st.format("%a %b %-d %H:%M"), en.format("%a %b %-d %H:%M"))
    }
}

fn event_glyph(e: &Value) -> String {
    match (s(e, "status"), s(e, "selfResponse")) {
        ("cancelled", _) | (_, "declined") => "✕".dimmed().to_string(),
        (_, "needsAction") => "?".yellow().to_string(),
        ("tentative", _) | (_, "tentative") => "◐".to_string(),
        _ => "●".green().to_string(),
    }
}

fn event_summary_line<Z: TimeZone>(e: &Value, tz: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let all_day = e.get("allDay").and_then(Value::as_bool) == Some(true);
    let title = if s(e, "title").is_empty() { "(no title)" } else { s(e, "title") };
    let mut extra: Vec<String> = Vec::new();
    if !s(e, "location").is_empty() {
        extra.push(s(e, "location").to_string());
    }
    if !s(e, "selfResponse").is_empty() {
        extra.push(s(e, "selfResponse").to_string());
    }
    if e.get("busy").and_then(Value::as_bool) == Some(false) {
        extra.push("free".into());
    }
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("  {}", extra.join(" · "))
    };
    format!(
        "{} {}  {}{}",
        event_glyph(e),
        fmt_range(s(e, "start"), s(e, "end"), all_day, tz).dimmed(),
        title.bold(),
        extra.dimmed()
    )
}

fn print_ids(e: &Value) {
    println!("      {} {}", "event   ".dimmed(), s(e, "id").cyan());
    println!("      {} {}", "calendar".dimmed(), s(e, "calendarRef").cyan());
}

fn render_events(body: &Value, limit: usize) {
    let events = body.get("events").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let window = format!("{} → {}", s(body, "timeMin"), s(body, "timeMax"));
    println!();
    if events.is_empty() {
        println!(
            "  {} no events between {window}. This is a confirmed read of the calendars, not a read failure.",
            "●".dimmed()
        );
    }
    for e in events.iter().take(limit) {
        println!("  {}", event_summary_line(e, &Local));
        print_ids(e);
    }
    println!();
    if events.len() > limit {
        eprintln!(
            "  {} showing {limit} of {} events — raise --limit or narrow --from/--to",
            "!".yellow(),
            events.len()
        );
    }
    for err in body.get("errors").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        eprintln!(
            "  {} calendar {} could not be read, so its events are missing: {}",
            "!".yellow(),
            s(err, "calendarRef"),
            s(err, "error")
        );
    }
}

fn render_event(e: &Value) {
    println!();
    println!("  {}", event_summary_line(e, &Local));
    let row = |k: &str, v: &str| {
        if !v.is_empty() {
            println!("      {} {v}", format!("{k:<9}").dimmed());
        }
    };
    row("status", s(e, "status"));
    row("organizer", s(e, "organizerEmail"));
    row("location", s(e, "location"));
    row("meeting", s(e, "conferenceUrl"));
    row("zone", s(e, "timeZone"));
    row("link", s(e, "htmlLink"));
    if !s(e, "recurringEventId").is_empty() {
        row("series", s(e, "recurringEventId"));
    }
    if let Some(people) = e.get("attendees").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        println!("      {}", "attendees".dimmed());
        for p in people {
            let mut tags: Vec<&str> = Vec::new();
            if !s(p, "responseStatus").is_empty() {
                tags.push(s(p, "responseStatus"));
            }
            if p.get("organizer").and_then(Value::as_bool) == Some(true) {
                tags.push("organizer");
            }
            if p.get("self").and_then(Value::as_bool) == Some(true) {
                tags.push("you");
            }
            if p.get("optional").and_then(Value::as_bool) == Some(true) {
                tags.push("optional");
            }
            println!("        {} {}  {}", "○".dimmed(), s(p, "email"), tags.join(" · ").dimmed());
        }
    }
    let description = s(e, "description").trim();
    if !description.is_empty() {
        println!("      {}", "description".dimmed());
        for line in description.lines() {
            println!("        {line}");
        }
    }
    print_ids(e);
    println!();
}

/// Each slot start, in the zone the search ran in.
fn slot_lines(body: &Value) -> Vec<String> {
    let tz = s(body, "timeZone").parse::<Tz>().ok();
    let minutes = body.get("durationMinutes").and_then(Value::as_i64).unwrap_or(0);
    body.get("slots")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .map(|slot| match (DateTime::parse_from_rfc3339(slot), tz) {
            (Ok(st), Some(tz)) => {
                let en: DateTime<FixedOffset> = st + chrono::Duration::minutes(minutes);
                fmt_range(&st.to_rfc3339(), &en.to_rfc3339(), false, &tz)
            }
            _ => slot.to_string(),
        })
        .collect()
}

fn render_slots(body: &Value) {
    let lines = slot_lines(body);
    println!();
    if lines.is_empty() {
        println!(
            "  {} no open {}-minute slot in that window and those working hours. This is a confirmed search, not a failure.",
            "●".dimmed(),
            body.get("durationMinutes").and_then(Value::as_i64).unwrap_or(0)
        );
    } else {
        println!("  {}", format!("open slots ({})", s(body, "timeZone")).dimmed());
    }
    for l in &lines {
        println!("  {} {l}", "○".green());
    }
    println!();
    let unchecked: Vec<&str> = body
        .get("unchecked")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !unchecked.is_empty() {
        eprintln!(
            "  {} not checked (their busy time is UNKNOWN, not free): {}",
            "!".yellow(),
            unchecked.join(", ")
        );
    }
}

/// One line after a write: what happened, then the ids to act on it next.
fn render_written(verb: &str, body: &Value) {
    let e = body.get("event").unwrap_or(body);
    println!();
    println!("  {} {verb}", "✓".green());
    println!("  {}", event_summary_line(e, &Local));
    if !s(e, "conferenceUrl").is_empty() {
        println!("      {} {}", "meeting ".dimmed(), s(e, "conferenceUrl"));
    }
    print_ids(e);
    println!();
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

async fn client() -> Result<smooth_api_client::SmoothApiClient> {
    require_user_session()
        .await
        .context("calendars belong to a person — run `smoo auth login` (an org API key has no calendar)")
}

/// The calendarRef to write to: `--calendar`, or the caller's one default.
async fn resolve_calendar(c: &smooth_api_client::SmoothApiClient, org: &str, calendar: Option<String>) -> Result<String> {
    if let Some(r) = calendar.as_deref().and_then(|r| non_blank(Some(r))) {
        return Ok(r.to_string());
    }
    let body = c
        .get(&format!("/organizations/{org}/calendars"))
        .await
        .map_err(|e| calendar_error(e, "GET calendars"))?;
    let (r, label) = pick_default_calendar(&body)?;
    eprintln!("  {} writing to {label}", "●".dimmed());
    Ok(r)
}

async fn read_event(c: &smooth_api_client::SmoothApiClient, path: &str) -> Result<Value> {
    let body = c.get(path).await.map_err(|e| calendar_error(e, "GET calendars/event"))?;
    Ok(body.get("event").cloned().unwrap_or(body))
}

fn gate(verb: &str, noun: &str, id: &str, org: &str, confirm: crate::destructive::Confirm) -> Result<bool> {
    crate::destructive::gate_with(
        &crate::destructive::Target {
            verb,
            noun,
            id,
            org,
            severity: crate::destructive::Severity::Standard,
        },
        confirm,
    )
}

#[allow(clippy::too_many_lines)]
pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::List { org, json } => {
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let body = c
                .get(&format!("/organizations/{o}/calendars"))
                .await
                .map_err(|e| calendar_error(e, "GET calendars"))?;
            if json {
                print_json(&body);
            } else {
                render_calendars(&body);
            }
        }
        Cmd::Events { cmd } => events(cmd).await?,
        Cmd::FindTime {
            duration,
            time_zone,
            from,
            to,
            calendars,
            attendees,
            workday_start,
            workday_end,
            days,
            max_slots,
            org,
            json,
        } => {
            let zone = match time_zone.as_deref().and_then(|z| non_blank(Some(z))) {
                Some(z) => z.to_string(),
                None => local_zone().context("could not tell this machine's time zone — pass --time-zone, e.g. America/New_York")?,
            };
            let body = find_time_body(&FindTime {
                duration,
                time_zone: &zone,
                from: from.as_deref(),
                to: to.as_deref(),
                calendars: &calendars,
                attendees: &attendees,
                workday_start: workday_start.as_deref(),
                workday_end: workday_end.as_deref(),
                days: &days,
                max_slots,
            })?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let res = c
                .post(&format!("/organizations/{o}/calendars/find-time"), Some(&body))
                .await
                .map_err(|e| calendar_error(e, "POST calendars/find-time"))?;
            if json {
                print_json(&res);
            } else {
                render_slots(&res);
            }
        }
        Cmd::Block {
            start,
            end,
            title,
            time_zone,
            calendar,
            org,
            json,
        } => {
            let mut body = block_body(&start, &end, title.as_deref(), time_zone.as_deref())?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            body.insert("calendarRef".into(), json!(resolve_calendar(&c, &o, calendar).await?));
            let res = c
                .post(&format!("/organizations/{o}/calendars/blocks"), Some(&Value::Object(body)))
                .await
                .map_err(|e| calendar_error(e, "POST calendars/blocks"))?;
            if json {
                print_json(&res);
            } else {
                render_written("time blocked", &res);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn events(cmd: EventsCmd) -> Result<()> {
    match cmd {
        EventsCmd::List {
            from,
            to,
            calendars,
            query,
            limit,
            org,
            json,
        } => {
            events_path("_", from.as_deref(), to.as_deref(), &calendars, query.as_deref())?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let body = c
                .get(&events_path(&o, from.as_deref(), to.as_deref(), &calendars, query.as_deref())?)
                .await
                .map_err(|e| calendar_error(e, "GET calendars/events"))?;
            if json {
                print_json(&body);
            } else {
                render_events(&body, limit.map_or(DEFAULT_EVENT_LIMIT, |n| n as usize));
            }
        }
        EventsCmd::Show { calendar, event, org, json } => {
            event_path("_", &calendar, &event)?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let body = c
                .get(&event_path(&o, &calendar, &event)?)
                .await
                .map_err(|e| calendar_error(e, "GET calendars/event"))?;
            if json {
                print_json(&body);
            } else {
                render_event(body.get("event").unwrap_or(&body));
            }
        }
        EventsCmd::Create {
            fields,
            attendees,
            calendar,
            org,
            json,
            confirm,
        } => {
            let mut body = create_body(&fields, &attendees)?;
            let invited = attendee_emails(&attendees, MAX_ATTENDEES)?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            body.insert("calendarRef".into(), json!(resolve_calendar(&c, &o, calendar).await?));
            // Attendees get a REAL invitation the moment this lands.
            if !invited.is_empty() {
                let noun = format!("{} to", guest_summary(&invited));
                let title = body.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
                if !gate("invite", &noun, &title, &o, confirm)? {
                    return Ok(());
                }
            }
            let res = c
                .post(&format!("/organizations/{o}/calendars/events"), Some(&Value::Object(body)))
                .await
                .map_err(|e| calendar_error(e, "POST calendars/events"))?;
            if json {
                print_json(&res);
            } else {
                render_written("event created", &res);
            }
        }
        EventsCmd::Update {
            calendar,
            event,
            fields,
            not_all_day,
            busy,
            no_conference,
            attendees,
            org,
            json,
            confirm,
        } => {
            let toggles = Toggles {
                not_all_day,
                busy,
                no_conference,
            };
            let body = patch_body(&fields, toggles, &attendees)?;
            event_path("_", &calendar, &event)?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let path = event_path(&o, &calendar, &event)?;
            // Guests are notified of any change, and a new list invites or
            // cancels people: read the event so the banner names them.
            let current = read_event(&c, &path).await?;
            let mut reached = guests(&current);
            for a in attendee_emails(&attendees, MAX_ATTENDEES)? {
                if !reached.contains(&a) {
                    reached.push(a);
                }
            }
            if !reached.is_empty() {
                let noun = format!("event, notifying {}:", guest_summary(&reached));
                if !gate("update", &noun, s(&current, "title"), &o, confirm)? {
                    return Ok(());
                }
            }
            let res = c.patch(&path, &body).await.map_err(|e| calendar_error(e, "PATCH calendars/event"))?;
            if json {
                print_json(&res);
            } else {
                render_written("event updated", &res);
            }
        }
        EventsCmd::Rm { calendar, event, org, confirm } => {
            event_path("_", &calendar, &event)?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let path = event_path(&o, &calendar, &event)?;
            // Preview: the event, read back, so the operator sees what goes.
            let current = read_event(&c, &path).await?;
            render_event(&current);
            let who = guests(&current);
            let noun = if who.is_empty() {
                "calendar event".to_string()
            } else {
                format!("calendar event, cancelling for {}:", guest_summary(&who))
            };
            if gate("delete", &noun, s(&current, "title"), &o, confirm)? {
                c.delete(&path).await.map_err(|e| calendar_error(e, "DELETE calendars/event"))?;
                println!("  {} event deleted", "✓".green());
                println!();
            }
        }
        EventsCmd::Respond {
            calendar,
            event,
            response,
            comment,
            org,
            json,
            confirm,
        } => {
            let body = respond_body(&calendar, &event, &response, comment.as_deref())?;
            let c = client().await?;
            let o = require_active_org(&c, org)?;
            let current = read_event(&c, &event_path(&o, &calendar, &event)?).await?;
            let organizer = s(&current, "organizerEmail");
            let noun = if organizer.is_empty() {
                format!("({response}) the invitation to")
            } else {
                format!("({response}, telling {organizer}) the invitation to")
            };
            if !gate("answer", &noun, s(&current, "title"), &o, confirm)? {
                return Ok(());
            }
            let res = c
                .post(&format!("/organizations/{o}/calendars/event/respond"), Some(&body))
                .await
                .map_err(|e| calendar_error(e, "POST calendars/event/respond"))?;
            if json {
                print_json(&res);
            } else {
                render_written(&format!("answered {response}"), &res);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use clap::Parser;

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

    fn fields(title: &str, start: &str, end: &str) -> EventFields {
        EventFields {
            title: Some(title.into()),
            start: Some(start.into()),
            end: Some(end.into()),
            ..EventFields::default()
        }
    }

    // ── parsing ──

    #[test]
    fn every_verb_parses() {
        assert!(matches!(parse(&["list", "--json"]).unwrap(), Cmd::List { json: true, .. }));
        assert!(matches!(
            parse(&["events", "list", "--calendar", "a,b", "--calendar", "c", "--limit", "5"]).unwrap(),
            Cmd::Events { cmd: EventsCmd::List { ref calendars, limit: Some(5), .. } } if calendars.len() == 3
        ));
        assert!(matches!(
            parse(&["event", "get", "--calendar", "r", "--event", "e", "--json"]).unwrap(),
            Cmd::Events {
                cmd: EventsCmd::Show { json: true, .. }
            }
        ));
        assert!(matches!(
            parse(&[
                "events", "create", "--title", "Sync", "--start", "2026-10-21T15:00:00Z", "--end", "2026-10-21T15:30:00Z", "--attendee", "a@x.co,b@x.co",
                "--conference", "--yes"
            ])
            .unwrap(),
            Cmd::Events { cmd: EventsCmd::Create { ref attendees, ref fields, confirm, .. } } if attendees.len() == 2 && fields.conference && confirm.yes
        ));
        assert!(matches!(
            parse(&["events", "update", "--calendar", "r", "--event", "e", "--title", "New", "--busy"]).unwrap(),
            Cmd::Events {
                cmd: EventsCmd::Update { busy: true, .. }
            }
        ));
        assert!(matches!(
            parse(&["events", "delete", "--calendar", "r", "--event", "e", "--dry-run"]).unwrap(),
            Cmd::Events { cmd: EventsCmd::Rm { confirm, .. } } if confirm.dry_run
        ));
        assert!(matches!(
            parse(&[
                "events",
                "rsvp",
                "--calendar",
                "r",
                "--event",
                "e",
                "--response",
                "declined",
                "--comment",
                "conflict"
            ])
            .unwrap(),
            Cmd::Events {
                cmd: EventsCmd::Respond { .. }
            }
        ));
        assert!(matches!(
            parse(&["find-time", "--duration", "30", "--tz", "America/New_York", "--days", "1,3", "--attendee", "p@x.co"]).unwrap(),
            Cmd::FindTime { duration: 30, ref days, .. } if days == &[1, 3]
        ));
        assert!(matches!(
            parse(&["block", "--start", "2026-10-21T13:00:00Z", "--end", "2026-10-21T14:00:00Z"]).unwrap(),
            Cmd::Block { calendar: None, .. }
        ));
    }

    #[test]
    fn required_ids_and_bounds_are_enforced_by_clap() {
        assert!(parse(&["events", "show", "--event", "e"]).is_err(), "--calendar is required");
        assert!(parse(&["events", "rm", "--calendar", "r"]).is_err(), "--event is required");
        assert!(parse(&["events", "respond", "--calendar", "r", "--event", "e", "--response", "maybe"]).is_err());
        assert!(parse(&["find-time", "--duration", "4"]).is_err());
        assert!(parse(&["find-time", "--duration", "481"]).is_err());
        assert!(parse(&["find-time"]).is_err(), "--duration is required");
        assert!(parse(&["find-time", "--duration", "30", "--max-slots", "51"]).is_err());
        assert!(parse(&["events", "list", "--limit", "0"]).is_err());
        assert!(
            parse(&["events", "update", "--calendar", "r", "--event", "e", "--free", "--busy"]).is_err(),
            "--free and --busy conflict"
        );
        assert!(parse(&["events", "update", "--calendar", "r", "--event", "e", "--all-day", "--not-all-day"]).is_err());
        assert!(parse(&["events", "update", "--calendar", "r", "--event", "e", "--conference", "--no-conference"]).is_err());
    }

    // ── request building ──

    #[test]
    fn events_path_carries_only_what_was_passed_and_encodes_refs() {
        assert_eq!(events_path("o1", None, None, &[], None).unwrap(), "/organizations/o1/calendars/events");
        assert_eq!(
            events_path(
                "o1",
                Some("2026-10-21T00:00:00Z"),
                Some("2026-10-28T00:00:00-04:00"),
                &["eyJ+/=".into(), " r2 ".into(), " ".into()],
                Some("1:1 sync")
            )
            .unwrap(),
            "/organizations/o1/calendars/events?from=2026-10-21T00%3A00%3A00Z&to=2026-10-28T00%3A00%3A00-04%3A00&q=1%3A1%20sync&calendarRefs=eyJ%2B%2F%3D%2Cr2"
        );
        assert!(events_path("o1", Some("tomorrow"), None, &[], None).is_err());
        assert!(events_path("o1", Some("2026-10-22T00:00:00Z"), Some("2026-10-21T00:00:00Z"), &[], None).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("r{i}")).collect();
        assert!(events_path("o1", None, None, &many, None).is_err(), "at most 20 calendars");
    }

    #[test]
    fn event_path_puts_both_ids_in_the_query_encoded() {
        assert_eq!(
            event_path("o1", "eyJbImdvb2dsZSJd", "AAMk+a/b==").unwrap(),
            "/organizations/o1/calendars/event?calendarRef=eyJbImdvb2dsZSJd&eventId=AAMk%2Ba%2Fb%3D%3D"
        );
        assert!(event_path("o1", " ", "e").is_err());
        assert!(event_path("o1", "r", "").is_err());
    }

    #[test]
    fn create_body_requires_title_start_end_and_shapes_attendees() {
        let b = create_body(
            &EventFields {
                location: Some(" HQ ".into()),
                conference: true,
                time_zone: Some("America/New_York".into()),
                ..fields(" Sync ", "2026-10-21T15:00:00-04:00", "2026-10-21T15:30:00-04:00")
            },
            &[" a@x.co ".into(), String::new()],
        )
        .unwrap();
        assert_eq!(
            Value::Object(b),
            json!({
                "title": "Sync",
                "start": "2026-10-21T15:00:00-04:00",
                "end": "2026-10-21T15:30:00-04:00",
                "timeZone": "America/New_York",
                "location": "HQ",
                "conference": true,
                "attendees": [{ "email": "a@x.co" }],
            })
        );
        assert!(create_body(&fields("", "2026-10-21T15:00:00Z", "2026-10-21T16:00:00Z"), &[]).is_err());
        assert!(create_body(
            &EventFields {
                end: None,
                ..fields("x", "2026-10-21T15:00:00Z", "")
            },
            &[]
        )
        .is_err());
        assert!(
            create_body(&fields("x", "2026-10-21 15:00", "2026-10-21T16:00:00Z"), &[]).is_err(),
            "not RFC3339"
        );
        assert!(
            create_body(&fields("x", "2026-10-21T15:00:00Z", "2026-10-21T15:00:00Z"), &[]).is_err(),
            "end == start"
        );
        assert!(create_body(&fields("x", "2026-10-21T15:00:00Z", "2026-10-21T16:00:00Z"), &["not an email".into()]).is_err());
    }

    #[test]
    fn attendees_must_be_addresses_and_are_capped() {
        assert!(attendee_emails(&["a@b".into()], 5).is_err(), "no dot in the domain");
        assert!(attendee_emails(&["@b.co".into()], 5).is_err());
        assert!(attendee_emails(&["a b@c.co".into()], 5).is_err());
        assert_eq!(attendee_emails(&["a@b.co".into()], 5).unwrap(), vec!["a@b.co"]);
        let many: Vec<String> = (0..=MAX_ATTENDEES).map(|i| format!("p{i}@x.co")).collect();
        assert!(attendee_emails(&many, MAX_ATTENDEES).is_err());
    }

    #[test]
    fn patch_body_sends_only_changes_and_both_directions_of_each_toggle() {
        let f = EventFields {
            title: Some("New".into()),
            ..EventFields::default()
        };
        assert_eq!(patch_body(&f, Toggles::default(), &[]).unwrap(), json!({ "title": "New" }));

        let t = Toggles {
            not_all_day: true,
            busy: true,
            no_conference: true,
        };
        assert_eq!(
            patch_body(&EventFields::default(), t, &[]).unwrap(),
            json!({ "allDay": false, "free": false, "conference": false })
        );
        let f = EventFields {
            all_day: true,
            free: true,
            conference: true,
            location: Some(String::new()),
            ..EventFields::default()
        };
        assert_eq!(
            patch_body(&f, Toggles::default(), &["n@x.co".into()]).unwrap(),
            json!({ "allDay": true, "free": true, "conference": true, "location": "", "attendees": [{ "email": "n@x.co" }] })
        );
        assert!(patch_body(&EventFields::default(), Toggles::default(), &[]).is_err(), "empty patch refused");
        let f = EventFields {
            start: Some("2026-10-21T16:00:00Z".into()),
            end: Some("2026-10-21T15:00:00Z".into()),
            ..EventFields::default()
        };
        assert!(patch_body(&f, Toggles::default(), &[]).is_err(), "end before start");
    }

    #[test]
    fn block_and_respond_bodies() {
        assert_eq!(
            Value::Object(block_body("2026-10-21T13:00:00Z", "2026-10-21T14:00:00Z", Some(" Deep work "), None).unwrap()),
            json!({ "start": "2026-10-21T13:00:00Z", "end": "2026-10-21T14:00:00Z", "title": "Deep work" })
        );
        assert!(block_body("2026-10-21T14:00:00Z", "2026-10-21T13:00:00Z", None, None).is_err());
        assert_eq!(
            respond_body("r", "e", "tentative", Some("  ")).unwrap(),
            json!({ "calendarRef": "r", "eventId": "e", "response": "tentative" })
        );
        assert_eq!(respond_body("r", "e", "accepted", Some("see you")).unwrap()["comment"], "see you");
        assert!(respond_body("r", "e", "needsAction", None).is_err());
    }

    #[test]
    fn find_time_body_matches_the_route_and_validates() {
        let ft = |tz: &'static str| FindTime {
            duration: 30,
            time_zone: tz,
            from: None,
            to: None,
            calendars: &[],
            attendees: &[],
            workday_start: None,
            workday_end: None,
            days: &[],
            max_slots: None,
        };
        assert_eq!(
            find_time_body(&ft("America/New_York")).unwrap(),
            json!({ "durationMinutes": 30, "timeZone": "America/New_York" })
        );
        assert!(find_time_body(&ft("Mars/Olympus")).is_err());

        let cals = vec!["r1".to_string()];
        let people = vec!["p@x.co".to_string()];
        let body = find_time_body(&FindTime {
            from: Some("2026-10-21T00:00:00Z"),
            to: Some("2026-10-24T00:00:00Z"),
            calendars: &cals,
            attendees: &people,
            workday_start: Some("08:30"),
            workday_end: Some("24:00"),
            days: &[5, 1, 1],
            max_slots: Some(3),
            ..ft("Europe/London")
        })
        .unwrap();
        assert_eq!(
            body,
            json!({
                "durationMinutes": 30, "timeZone": "Europe/London",
                "from": "2026-10-21T00:00:00Z", "to": "2026-10-24T00:00:00Z",
                "calendarRefs": ["r1"], "attendeeEmails": ["p@x.co"],
                "workdayStartMinute": 510, "workdayEndMinute": 1440,
                "days": [1, 5], "maxSlots": 3,
            })
        );
        assert!(find_time_body(&FindTime { days: &[7], ..ft("UTC") }).is_err());
        assert!(
            find_time_body(&FindTime {
                workday_start: Some("18:00"),
                ..ft("UTC")
            })
            .is_err(),
            "start after the default 17:00 end"
        );
        assert!(find_time_body(&FindTime {
            workday_start: Some("9am"),
            ..ft("UTC")
        })
        .is_err());
    }

    #[test]
    fn hhmm_parses_and_bounds() {
        assert_eq!(hhmm("x", "09:00").unwrap(), 540);
        assert_eq!(hhmm("x", "24:00").unwrap(), 1440);
        assert!(hhmm("x", "24:01").is_err());
        assert!(hhmm("x", "12:60").is_err());
        assert!(hhmm("x", "1200").is_err());
    }

    #[test]
    fn local_zone_reads_the_localtime_link() {
        assert_eq!(
            zone_from_localtime_target("/var/db/timezone/zoneinfo/America/Indiana/Indianapolis").as_deref(),
            Some("America/Indiana/Indianapolis")
        );
        assert_eq!(zone_from_localtime_target("/usr/share/zoneinfo/UTC").as_deref(), Some("UTC"));
        assert_eq!(zone_from_localtime_target("/etc/localtime"), None);
        assert_eq!(zone_from_localtime_target("/usr/share/zoneinfo/Not/AZone"), None);
    }

    // ── default calendar ──

    fn cal(name: &str, own: bool, default: bool, write: bool) -> Value {
        json!({ "calendarRef": format!("ref-{name}"), "name": name, "accountEmail": "me@x.co", "provider": "google",
                "own": own, "isDefault": default, "canWrite": write, "scope": "user" })
    }

    #[test]
    fn default_calendar_is_the_one_own_writable_default() {
        let body = json!({ "calendars": [cal("team", false, true, true), cal("me", true, true, true), cal("hol", true, false, true)] });
        assert_eq!(pick_default_calendar(&body).unwrap().0, "ref-me");

        let two = json!({ "calendars": [cal("g", true, true, true), cal("m", true, true, true)] });
        let err = pick_default_calendar(&two).unwrap_err().to_string();
        assert!(err.contains("2 default calendars") && err.contains("--calendar"), "{err}");

        let none = json!({ "calendars": [cal("ro", true, true, false)] });
        assert!(pick_default_calendar(&none).unwrap_err().to_string().contains("--calendar"));

        let empty = json!({ "calendars": [] });
        assert!(pick_default_calendar(&empty).unwrap_err().to_string().contains("smoo integrations connect"));
    }

    // ── guests + errors ──

    #[test]
    fn guests_exclude_the_calendar_itself_and_summarise() {
        let e = json!({ "attendees": [
            { "email": "me@x.co", "self": true }, { "email": "a@x.co" }, { "email": "b@x.co" },
            { "email": "c@x.co" }, { "email": "d@x.co" }
        ]});
        let g = guests(&e);
        assert_eq!(g, vec!["a@x.co", "b@x.co", "c@x.co", "d@x.co"]);
        assert_eq!(guest_summary(&g), "4 attendees (a@x.co, b@x.co, c@x.co, +1 more)");
        assert_eq!(guest_summary(&g[..1]), "1 attendee (a@x.co)");
        assert!(guests(&json!({})).is_empty());
    }

    #[test]
    fn errors_name_the_next_step() {
        let e = calendar_error(anyhow::anyhow!("HTTP 409 Conflict: calendar_not_connected"), "GET x");
        assert!(format!("{e:#}").contains("smoo integrations connect google"));
        let e = calendar_error(anyhow::anyhow!("HTTP 404 Not Found: Event not found."), "GET x");
        let msg = format!("{e:#}");
        assert!(msg.contains("smoo calendar list") && msg.contains("Event not found"), "{msg}");
        let e = calendar_error(anyhow::anyhow!("HTTP 500 boom"), "GET x");
        let msg = format!("{e:#}");
        assert!(msg.starts_with("GET x") && msg.contains("HTTP 500"), "{msg}");
    }

    // ── rendering ──

    #[test]
    fn ranges_format_in_the_given_zone() {
        assert_eq!(
            fmt_range("2026-10-21T15:00:00-04:00", "2026-10-21T15:30:00-04:00", false, &Utc),
            "Wed Oct 21 19:00–19:30"
        );
        assert_eq!(
            fmt_range("2026-10-21T23:30:00Z", "2026-10-22T00:30:00Z", false, &Utc),
            "Wed Oct 21 23:30 – Thu Oct 22 00:30"
        );
        assert_eq!(
            fmt_range("2026-10-21T00:00:00-04:00", "2026-10-22T00:00:00-04:00", true, &Utc),
            "Wed Oct 21 (all day)"
        );
        assert_eq!(
            fmt_range("2026-10-21T00:00:00Z", "2026-10-24T00:00:00Z", true, &Utc),
            "Wed Oct 21 – Fri Oct 23 (all day)"
        );
        assert_eq!(fmt_range("soon", "later", false, &Utc), "soon – later");
    }

    #[test]
    fn slots_render_in_the_search_zone() {
        let body = json!({ "slots": ["2026-10-21T13:00:00Z", "junk"], "durationMinutes": 30, "timeZone": "America/New_York", "unchecked": [] });
        assert_eq!(slot_lines(&body), vec!["Wed Oct 21 09:00–09:30".to_string(), "junk".to_string()]);
        assert!(slot_lines(&json!({ "slots": [] })).is_empty());
    }

    #[test]
    fn calendar_labels_and_owners() {
        let mine = cal("Work", true, true, true);
        assert_eq!(calendar_label(&mine), "Work <me@x.co> [google]");
        assert_eq!(calendar_owner(&mine), "own");
        let org = json!({ "name": "Sales", "provider": "microsoft", "own": false, "scope": "org" });
        assert_eq!(calendar_label(&org), "Sales [microsoft]");
        assert_eq!(calendar_owner(&org), "org");
        let shared = json!({ "name": "pat@x.co", "accountEmail": "pat@x.co", "provider": "apple", "own": false, "scope": "user" });
        assert_eq!(calendar_label(&shared), "pat@x.co [apple]");
        assert_eq!(calendar_owner(&shared), "shared");
    }

    #[test]
    fn summary_line_carries_title_location_and_answer() {
        let e = json!({ "title": "Sync", "start": "2026-10-21T15:00:00Z", "end": "2026-10-21T15:30:00Z",
                        "location": "HQ", "selfResponse": "accepted", "busy": false, "status": "confirmed" });
        let line = event_summary_line(&e, &Utc);
        for want in ["Sync", "HQ", "accepted", "free", "Wed Oct 21 15:00–15:30"] {
            assert!(line.contains(want), "{want:?} missing from {line:?}");
        }
        let untitled = event_summary_line(&json!({ "start": "x", "end": "y" }), &Utc);
        assert!(untitled.contains("(no title)"));
    }

    #[test]
    fn renderers_do_not_panic_on_odd_shapes() {
        render_calendars(&json!({}));
        render_calendars(&json!({ "calendars": [cal("a", true, true, true), { "status": "error", "provider": "google" }] }));
        render_events(&json!({ "events": [], "errors": [{ "calendarRef": "r", "error": "boom" }] }), 25);
        render_events(&json!({ "events": [{ "id": "1" }, { "id": "2" }] }), 1);
        render_event(&json!({ "title": "x", "attendees": [{ "email": "a@x.co", "organizer": true }], "description": "a\nb" }));
        render_slots(&json!({ "slots": [], "durationMinutes": 30, "unchecked": ["p@x.co"] }));
        render_written("done", &json!({ "event": { "id": "1", "conferenceUrl": "https://meet" } }));
    }
}
