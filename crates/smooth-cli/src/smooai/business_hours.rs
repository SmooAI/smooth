//! `th smoo calling hours show|create|delete|holidays get|set` — the rest of the
//! business-hours surface (SMOODEV-4052). `hours list|update` stay in
//! `calling_config.rs`; [`super::calling_config::HoursCmd`] dispatches here.
//!
//! The CLI twin of the hosted MCP's `business_hours_get` / `business_hours_create` /
//! `business_hours_delete` / `business_hours_holidays_set` and the copilot's
//! `business_hours.*`. Every verb calls the api-prime route the Calling settings
//! page calls (`/organizations/{org}/calling/business-hours…`) with your own
//! sign-in, so the route decides: `calling.use` to read, `calling.admin` to write,
//! plus the `telephony` product. A 403 is reported as "admin only".
//!
//! `holidays set --us-holidays` adds the dashboard's 14-entry "Add US holidays"
//! preset ([`US_HOLIDAYS`], a copy of smooai's
//! `apps/web/components/calling/admin/calling-admin-utils.ts` and
//! `tools_calling_admin::US_HOLIDAYS`) to the schedule, skipping entries it
//! already has, so running it twice adds nothing.

use std::fmt::Write as _;

use anstream::println;
use anyhow::{bail, Context, Result};
use chrono::{Datelike, Duration, NaiveDate};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::ring_groups::{admin_only, group_id as uuid_id};
use super::{print_json, read_body, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// `MAX_NAME` in smooai api-prime `handlers/calling/business_hours.rs`.
pub const MAX_NAME: usize = 100;
/// `MAX_WINDOWS` in api-prime.
pub const MAX_WINDOWS: usize = 70;
/// `MAX_HOLIDAYS` in api-prime.
pub const MAX_HOLIDAYS: usize = 366;
/// `HOLIDAY_KEYS` in api-prime: what a holiday in the `PUT` body may carry.
pub const HOLIDAY_KEYS: &[&str] = &["date", "name", "recursAnnually", "rule", "observed", "closedAllDay", "startMinute", "endMinute"];

const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

#[derive(Subcommand)]
pub enum HolidaysCmd {
    /// A schedule's holidays.
    #[command(visible_alias = "show")]
    Get {
        /// The schedule id (from `calling hours list`).
        hours_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Replace a schedule's holidays (needs calling admin). `--body` is the
    /// COMPLETE list; `--us-holidays` adds the US preset to it (or to the
    /// current list when there is no `--body`); `--clear` empties it.
    Set {
        /// The schedule id (from `calling hours list`).
        hours_id: String,
        /// The full list as JSON from a file (`-` for stdin):
        /// `[{"date":"2026-12-25","name":"Christmas","recursAnnually":true}, …]` or `{"holidays": [...]}`.
        #[arg(long, conflicts_with = "clear")]
        body: Option<String>,
        /// Add the 14 US holidays (federal holidays plus Christmas Eve, the day
        /// after Thanksgiving and New Year's Eve) the list does not already have.
        #[arg(long = "us-holidays")]
        us_holidays: bool,
        /// Remove every holiday.
        #[arg(long, conflicts_with = "us_holidays")]
        clear: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

// ── validation ───────────────────────────────────────────────────────────────

/// `HH:MM` (or `24:00`) as minutes from midnight.
fn minute(raw: &str) -> Result<i64> {
    let (h, m) = raw.trim().split_once(':').with_context(|| format!("{raw:?} is not a time like 09:00"))?;
    let (h, m): (i64, i64) = (
        h.parse().with_context(|| format!("bad hour in {raw:?}"))?,
        m.parse().with_context(|| format!("bad minute in {raw:?}"))?,
    );
    if !(0..=24).contains(&h) || !(0..60).contains(&m) || (h == 24 && m != 0) {
        bail!("{raw:?} is not a time of day (00:00 to 24:00)");
    }
    Ok(h * 60 + m)
}

fn day(raw: &str) -> Result<usize> {
    let d = raw.trim().to_ascii_lowercase();
    DAYS.iter()
        .position(|x| d.starts_with(x))
        .with_context(|| format!("{raw:?} is not a day (sun, mon, tue, wed, thu, fri, sat)"))
}

/// `--window` specs as the route's `windows`: `mon:09:00-17:00`, or a day range
/// `mon-fri:09:00-17:00`. Overlaps are left to api-prime to refuse.
pub fn windows(specs: &[String]) -> Result<Value> {
    let mut out = Vec::new();
    for spec in specs {
        let (days, times) = spec
            .split_once(':')
            .with_context(|| format!("--window {spec:?}: use DAY[-DAY]:HH:MM-HH:MM, e.g. mon-fri:09:00-17:00"))?;
        let (start, end) = times.split_once('-').with_context(|| format!("--window {spec:?}: the hours are HH:MM-HH:MM"))?;
        let (start, end) = (minute(start)?, minute(end)?);
        if start >= end {
            bail!("--window {spec:?}: the start must be before the end");
        }
        let (first, last) = match days.split_once('-') {
            Some((a, b)) => (day(a)?, day(b)?),
            None => (day(days)?, day(days)?),
        };
        if first > last {
            bail!("--window {spec:?}: the day range runs backwards (weeks start on sun)");
        }
        for d in first..=last {
            out.push(json!({ "day": d, "startMinute": start, "endMinute": end }));
        }
    }
    if out.len() > MAX_WINDOWS {
        bail!("a schedule can have at most {MAX_WINDOWS} windows");
    }
    Ok(Value::Array(out))
}

/// The `POST` body for `create`.
pub fn create_body(name: &str, timezone: &str, window_specs: &[String], is_default: bool) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        bail!("the name must be 1 to {MAX_NAME} characters");
    }
    let tz = timezone.trim();
    if tz.parse::<chrono_tz::Tz>().is_err() {
        bail!("--timezone {tz:?} must be an IANA zone name, e.g. America/Indiana/Indianapolis");
    }
    let mut body = Map::new();
    body.insert("name".into(), json!(name));
    body.insert("timezone".into(), json!(tz));
    if !window_specs.is_empty() {
        body.insert("windows".into(), windows(window_specs)?);
    }
    if is_default {
        body.insert("isDefault".into(), json!(true));
    }
    Ok(Value::Object(body))
}

/// A caller-supplied holiday list: at most [`MAX_HOLIDAYS`] objects with only
/// [`HOLIDAY_KEYS`] and a `YYYY-MM-DD` date. The rest is api-prime's to refuse.
pub fn holidays_list(v: Value) -> Result<Vec<Value>> {
    const SHAPE: &str = "--body must be a JSON array of holidays or {\"holidays\": [...]}";
    let items = match v {
        Value::Array(a) => a,
        Value::Object(mut m) => match m.remove("holidays") {
            Some(Value::Array(a)) if m.is_empty() => a,
            _ => bail!(SHAPE),
        },
        _ => bail!(SHAPE),
    };
    if items.len() > MAX_HOLIDAYS {
        bail!("a schedule can have at most {MAX_HOLIDAYS} holidays");
    }
    for (i, h) in items.iter().enumerate() {
        let Some(h) = h.as_object() else {
            bail!("holidays[{i}] must be an object with a date");
        };
        if let Some(k) = h.keys().find(|k| !HOLIDAY_KEYS.contains(&k.as_str())) {
            bail!("holidays[{i}].{k} is not a holiday field — use {}", HOLIDAY_KEYS.join(", "));
        }
        let date = h.get("date").and_then(Value::as_str).unwrap_or_default();
        if NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
            bail!("holidays[{i}].date must be YYYY-MM-DD");
        }
    }
    Ok(items)
}

/// A holiday row from a `GET` trimmed to what the `PUT` accepts.
#[must_use]
pub fn holiday_input(row: &Value) -> Value {
    let mut out = Map::new();
    for key in HOLIDAY_KEYS {
        if let Some(v) = row.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

// ── the US holidays preset ───────────────────────────────────────────────────

/// How a [`UsHoliday`] repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A fixed month/day; `observed` also closes the nearest weekday.
    Fixed { month: u32, day: u32, observed: bool },
    /// The `nth` (`-1` = last) `weekday` (0 = Sunday) of `month`, plus `offset` days.
    Rule { month: u32, weekday: u32, nth: i32, offset: i64 },
}

/// One entry of the "Add US holidays" checklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsHoliday {
    pub name: &'static str,
    pub kind: Kind,
}

const fn fixed(name: &'static str, month: u32, day: u32, observed: bool) -> UsHoliday {
    UsHoliday {
        name,
        kind: Kind::Fixed { month, day, observed },
    }
}

const fn rule(name: &'static str, month: u32, weekday: u32, nth: i32, offset: i64) -> UsHoliday {
    UsHoliday {
        name,
        kind: Kind::Rule { month, weekday, nth, offset },
    }
}

/// The dashboard's `US_HOLIDAYS`, in calendar order.
pub const US_HOLIDAYS: &[UsHoliday] = &[
    fixed("New Year's Day", 1, 1, true),
    rule("Martin Luther King Jr. Day", 1, 1, 3, 0),
    rule("Presidents' Day", 2, 1, 3, 0),
    rule("Memorial Day", 5, 1, -1, 0),
    fixed("Juneteenth", 6, 19, true),
    fixed("Independence Day", 7, 4, true),
    rule("Labor Day", 9, 1, 1, 0),
    rule("Indigenous Peoples' Day / Columbus Day", 10, 1, 2, 0),
    fixed("Veterans Day", 11, 11, true),
    rule("Thanksgiving", 11, 4, 4, 0),
    rule("Day after Thanksgiving", 11, 4, 4, 1),
    fixed("Christmas Eve", 12, 24, false),
    fixed("Christmas Day", 12, 25, true),
    fixed("New Year's Eve", 12, 31, false),
];

/// The date an nth-weekday rule lands on in `year`.
#[must_use]
pub fn rule_date_in(month: u32, weekday: u32, nth: i32, offset: i64, year: i32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let base = if nth == -1 {
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next - Duration::days(1);
        let back = (i64::from(last.weekday().num_days_from_sunday()) - i64::from(weekday) + 7) % 7;
        last - Duration::days(back)
    } else {
        let ahead = (i64::from(weekday) - i64::from(first.weekday().num_days_from_sunday()) + 7) % 7;
        let d = first + Duration::days(ahead + i64::from(nth - 1) * 7);
        if d.month() != month {
            return None;
        }
        d
    };
    Some(base + Duration::days(offset))
}

/// The holiday row a preset entry becomes; `date` is its next occurrence on or after `today`.
#[must_use]
pub fn us_holiday_input(h: &UsHoliday, today: NaiveDate) -> Value {
    let year = today.year();
    match h.kind {
        Kind::Rule { month, weekday, nth, offset } => {
            let date = rule_date_in(month, weekday, nth, offset, year)
                .filter(|d| *d >= today)
                .or_else(|| rule_date_in(month, weekday, nth, offset, year + 1))
                .unwrap_or(today);
            let mut r = json!({ "kind": "nth_weekday", "month": month, "weekday": weekday, "nth": nth });
            if offset != 0 {
                r["offsetDays"] = json!(offset);
            }
            json!({ "date": date.to_string(), "name": h.name, "recursAnnually": false, "rule": r, "observed": false,
                    "closedAllDay": true, "startMinute": null, "endMinute": null })
        }
        Kind::Fixed { month, day, observed } => {
            let date = NaiveDate::from_ymd_opt(year, month, day)
                .filter(|d| *d >= today)
                .or_else(|| NaiveDate::from_ymd_opt(year + 1, month, day))
                .unwrap_or(today);
            json!({ "date": date.to_string(), "name": h.name, "recursAnnually": true, "rule": null, "observed": observed,
                    "closedAllDay": true, "startMinute": null, "endMinute": null })
        }
    }
}

/// Is this preset entry already on the schedule? Same name, same rule, same
/// yearly month/day — or the same exact date, which the route would refuse.
#[must_use]
pub fn us_holiday_exists(h: &UsHoliday, existing: &[Value], today: NaiveDate) -> bool {
    let next = us_holiday_input(h, today);
    existing.iter().any(|e| {
        let e_name = e.get("name").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
        if !e_name.is_empty() && e_name == h.name.to_lowercase() {
            return true;
        }
        let e_rule = e.get("rule").filter(|r| !r.is_null());
        match h.kind {
            Kind::Rule { month, weekday, nth, offset } => e_rule.is_some_and(|r| {
                let n = |k: &str| r.get(k).and_then(Value::as_i64);
                n("month") == Some(i64::from(month))
                    && n("weekday") == Some(i64::from(weekday))
                    && n("nth") == Some(i64::from(nth))
                    && n("offsetDays").unwrap_or(0) == offset
            }),
            Kind::Fixed { month, day, .. } => {
                let date = e
                    .get("date")
                    .and_then(Value::as_str)
                    .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
                let yearly = e.get("recursAnnually").and_then(Value::as_bool) == Some(true);
                e_rule.is_none() && ((yearly && date.is_some_and(|d| d.month() == month && d.day() == day)) || e.get("date") == next.get("date"))
            }
        }
    })
}

/// `existing` plus every preset entry it does not already have, and their names.
#[must_use]
pub fn with_us_holidays(existing: Vec<Value>, today: NaiveDate) -> (Vec<Value>, Vec<&'static str>) {
    let mut out = existing;
    let mut added = Vec::new();
    for h in US_HOLIDAYS {
        if !us_holiday_exists(h, &out, today) {
            out.push(us_holiday_input(h, today));
            added.push(h.name);
        }
    }
    (out, added)
}

// ── rendering ────────────────────────────────────────────────────────────────

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn hhmm(m: i64) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// One holiday as a line.
#[must_use]
pub fn holiday_line(h: &Value) -> String {
    let name = match s(h, "name") {
        "" => "(unnamed)",
        n => n,
    };
    let repeats = if h.get("rule").is_some_and(|r| !r.is_null()) {
        "yearly by rule"
    } else if h.get("recursAnnually").and_then(Value::as_bool) == Some(true) {
        "every year"
    } else {
        "once"
    };
    let mut out = format!("{} {name} ({repeats}", s(h, "date"));
    if h.get("observed").and_then(Value::as_bool) == Some(true) {
        out.push_str(", observed");
    }
    out.push(')');
    if h.get("closedAllDay").and_then(Value::as_bool) == Some(false) {
        let n = |k: &str| h.get(k).and_then(Value::as_i64).unwrap_or(0);
        let _ = write!(out, " open {}-{}", hhmm(n("startMinute")), hhmm(n("endMinute")));
    } else {
        out.push_str(" closed all day");
    }
    out
}

fn render_detail(h: &Value) {
    let default = if h.get("isDefault").and_then(Value::as_bool) == Some(true) {
        " [default]"
    } else {
        ""
    };
    println!(
        "\n  {} {}{} — {}",
        s(h, "name").bold(),
        format!("({})", s(h, "id")).dimmed(),
        default.green(),
        s(h, "timezone")
    );
    let windows = h.get("windows").and_then(Value::as_array).cloned().unwrap_or_default();
    if windows.is_empty() {
        println!("    {}", "no open windows — always after hours".dimmed());
    }
    for w in &windows {
        let n = |k: &str| w.get(k).and_then(Value::as_i64).unwrap_or(0);
        let d = usize::try_from(n("day")).ok().and_then(|d| DAYS.get(d)).copied().unwrap_or("?");
        println!("    {d} {}-{}", hhmm(n("startMinute")), hhmm(n("endMinute")));
    }
    let holidays = h.get("holidays").and_then(Value::as_array).cloned().unwrap_or_default();
    println!("    {} holiday(s)", holidays.len());
    for hol in &holidays {
        println!("      {}", holiday_line(hol));
    }
    println!();
}

// ── commands ─────────────────────────────────────────────────────────────────

fn hours_id(id: &str) -> Result<String> {
    uuid_id(id).map_err(|_| anyhow::anyhow!("schedule id must be a uuid (from `th smoo calling hours list`); got {:?}", id.trim()))
}

pub async fn show(hours_id_raw: &str, org: Option<String>, json: bool) -> Result<()> {
    let id = hours_id(hours_id_raw)?;
    let client = require_authed().await?;
    let o = require_active_org(&client, org)?;
    let body = client
        .get(&format!("/organizations/{o}/calling/business-hours/{id}"))
        .await
        .map_err(|e| admin_only(e, false))
        .context("GET calling/business-hours/:id")?;
    if json {
        print_json(&body);
    } else {
        render_detail(&body);
    }
    Ok(())
}

pub async fn create(name: &str, timezone: &str, window_specs: &[String], is_default: bool, org: Option<String>, confirm: Confirm) -> Result<()> {
    let body = create_body(name, timezone, window_specs, is_default)?;
    let client = require_authed().await?;
    let o = require_active_org(&client, org)?;
    let path = format!("/organizations/{o}/calling/business-hours");
    if confirm.dry_run {
        println!("\n  {} would POST {path}", "●".dimmed());
        print_json(&body);
    }
    let target = Target {
        verb: "create",
        noun: "business-hours schedule",
        id: name.trim(),
        org: &o,
        severity: Severity::Standard,
    };
    if crate::destructive::gate_with(&target, confirm)? {
        let created = client
            .post(&path, Some(&body))
            .await
            .map_err(|e| admin_only(e, true))
            .with_context(|| format!("POST {path}"))?;
        println!("\n  {} created — no number uses it until one is pointed at it", "✓".green());
        render_detail(&created);
    }
    Ok(())
}

pub async fn delete(hours_id_raw: &str, org: Option<String>, confirm: Confirm) -> Result<()> {
    let id = hours_id(hours_id_raw)?;
    let client = require_authed().await?;
    let o = require_active_org(&client, org)?;
    if crate::destructive::confirm_delete("business-hours schedule", &id, &o, confirm)? {
        let path = format!("/organizations/{o}/calling/business-hours/{id}");
        client
            .delete(&path)
            .await
            .map_err(|e| admin_only(e, true))
            .with_context(|| format!("DELETE {path} (refused while a phone number uses it)"))?;
        println!("\n  {} deleted business-hours schedule {id}\n", "✓".green());
    }
    Ok(())
}

pub async fn holidays(cmd: HolidaysCmd) -> Result<()> {
    match cmd {
        HolidaysCmd::Get { hours_id: id, org, json } => {
            let id = hours_id(&id)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/business-hours/{id}"))
                .await
                .map_err(|e| admin_only(e, false))
                .context("GET calling/business-hours/:id")?;
            let list = body.get("holidays").cloned().unwrap_or_else(|| json!([]));
            if json {
                print_json(&list);
                return Ok(());
            }
            let rows = list.as_array().cloned().unwrap_or_default();
            println!();
            if rows.is_empty() {
                println!("  {} {}", "●".dimmed(), "no holidays".dimmed());
            }
            for h in &rows {
                println!("  {} {}", "○".dimmed(), holiday_line(h));
            }
            println!();
        }
        HolidaysCmd::Set {
            hours_id: id,
            body,
            us_holidays,
            clear,
            org,
            confirm,
        } => {
            let id = hours_id(&id)?;
            let given = match (&body, clear) {
                (Some(path), _) => Some(holidays_list(read_body(path)?)?),
                (None, true) => Some(Vec::new()),
                (None, false) if us_holidays => None,
                (None, false) => bail!("pass the full list with --body, --us-holidays to add the US preset, or --clear"),
            };
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let path = format!("/organizations/{o}/calling/business-hours/{id}");
            let base = if let Some(list) = given {
                list
            } else {
                let current = client
                    .get(&path)
                    .await
                    .map_err(|e| admin_only(e, false))
                    .context("GET calling/business-hours/:id")?;
                current
                    .get("holidays")
                    .and_then(Value::as_array)
                    .map(|rows| rows.iter().map(holiday_input).collect())
                    .unwrap_or_default()
            };
            let (list, added) = if us_holidays {
                with_us_holidays(base, chrono::Local::now().date_naive())
            } else {
                (base, Vec::new())
            };
            if us_holidays {
                if added.is_empty() {
                    println!("\n  {} every US holiday is already on the schedule", "●".dimmed());
                } else {
                    println!("\n  {} adding {} US holiday(s): {}", "●".dimmed(), added.len(), added.join(", "));
                }
            }
            let put_path = format!("{path}/holidays");
            let body = json!({ "holidays": list });
            if confirm.dry_run {
                println!("\n  {} would PUT {put_path}", "●".dimmed());
                print_json(&body);
            }
            let target = Target {
                verb: "replace the holidays of",
                noun: "business-hours schedule",
                id: &id,
                org: &o,
                severity: Severity::Standard,
            };
            if crate::destructive::gate_with(&target, confirm)? {
                let updated = client
                    .put(&put_path, &body)
                    .await
                    .map_err(|e| admin_only(e, true))
                    .with_context(|| format!("PUT {put_path}"))?;
                println!("\n  {} holidays replaced", "✓".green());
                render_detail(&updated);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn window_specs_expand_day_ranges() {
        assert_eq!(
            windows(&["mon-wed:09:00-17:00".into(), "sat:10:00-12:30".into()]).unwrap(),
            json!([
                { "day": 1, "startMinute": 540, "endMinute": 1020 },
                { "day": 2, "startMinute": 540, "endMinute": 1020 },
                { "day": 3, "startMinute": 540, "endMinute": 1020 },
                { "day": 6, "startMinute": 600, "endMinute": 750 }
            ])
        );
        assert_eq!(windows(&["sunday:00:00-24:00".into()]).unwrap()[0]["endMinute"], json!(1440));
        for bad in [
            "mon",
            "mon:9-17",
            "mon:17:00-09:00",
            "fri-mon:09:00-17:00",
            "xyz:09:00-10:00",
            "mon:24:30-25:00",
            "mon:09:60-10:00",
        ] {
            assert!(windows(&[bad.into()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn create_body_requires_a_name_and_an_iana_zone() {
        assert_eq!(
            create_body(" Main ", "America/New_York", &["mon:09:00-17:00".into()], true).unwrap(),
            json!({ "name": "Main", "timezone": "America/New_York", "isDefault": true, "windows": [{ "day": 1, "startMinute": 540, "endMinute": 1020 }] })
        );
        assert_eq!(create_body("Main", "UTC", &[], false).unwrap(), json!({ "name": "Main", "timezone": "UTC" }));
        assert!(create_body("", "UTC", &[], false).is_err());
        assert!(create_body(&"x".repeat(MAX_NAME + 1), "UTC", &[], false).is_err());
        assert!(create_body("Main", "Eastern", &[], false).is_err());
    }

    #[test]
    fn holiday_lists_are_shape_checked() {
        assert_eq!(holidays_list(json!({ "holidays": [] })).unwrap(), Vec::<Value>::new());
        assert_eq!(holidays_list(json!([{ "date": "2026-12-25", "name": "Christmas" }])).unwrap().len(), 1);
        assert!(holidays_list(json!([{ "date": "12/25" }])).is_err());
        assert!(holidays_list(json!([{ "date": "2026-12-25", "id": "x" }])).is_err());
        assert!(holidays_list(json!({ "holidays": [], "extra": 1 })).is_err());
        assert!(holidays_list(json!("x")).is_err());
        let many: Vec<Value> = (0..=MAX_HOLIDAYS).map(|_| json!({ "date": "2026-01-01" })).collect();
        assert!(holidays_list(Value::Array(many)).is_err());
        let trimmed = holiday_input(&json!({ "id": "x", "createdAt": "t", "date": "2026-12-25", "name": "Xmas" }));
        assert_eq!(trimmed, json!({ "date": "2026-12-25", "name": "Xmas" }));
    }

    #[test]
    fn the_preset_is_the_dashboard_checklist() {
        assert_eq!(US_HOLIDAYS.len(), 14);
        // 2026: MLK Jan 19, Memorial May 25, Labor Sep 7, Thanksgiving Nov 26 (+1 = 27).
        assert_eq!(rule_date_in(1, 1, 3, 0, 2026), Some(date("2026-01-19")));
        assert_eq!(rule_date_in(5, 1, -1, 0, 2026), Some(date("2026-05-25")));
        assert_eq!(rule_date_in(9, 1, 1, 0, 2026), Some(date("2026-09-07")));
        assert_eq!(rule_date_in(11, 4, 4, 0, 2026), Some(date("2026-11-26")));
        assert_eq!(rule_date_in(11, 4, 4, 1, 2026), Some(date("2026-11-27")));
        assert_eq!(rule_date_in(2, 1, 5, 0, 2026), None);
        let today = date("2026-10-20");
        let tg = us_holiday_input(&US_HOLIDAYS[9], today);
        assert_eq!(tg["date"], json!("2026-11-26"));
        assert_eq!(tg["rule"], json!({ "kind": "nth_weekday", "month": 11, "weekday": 4, "nth": 4 }));
        let ny = us_holiday_input(&US_HOLIDAYS[0], today);
        assert_eq!((ny["date"].clone(), ny["recursAnnually"].clone()), (json!("2027-01-01"), json!(true)));
        let all: Vec<Value> = US_HOLIDAYS.iter().map(|h| us_holiday_input(h, today)).collect();
        assert!(holidays_list(Value::Array(all)).is_ok());
    }

    #[test]
    fn adding_the_preset_twice_adds_nothing_the_second_time() {
        let today = date("2026-10-20");
        let existing = vec![json!({ "date": "2026-12-25", "name": "xmas", "recursAnnually": true, "rule": null })];
        let (list, added) = with_us_holidays(existing, today);
        assert_eq!((list.len(), added.len()), (14, 13));
        assert!(!added.contains(&"Christmas Day"));
        let (again, added) = with_us_holidays(list.clone(), today);
        assert!(added.is_empty());
        assert_eq!(again, list);
        let (_, added) = with_us_holidays(
            vec![
                json!({ "date": "2027-03-01", "name": " THANKSGIVING " }),
                json!({ "date": "2026-12-24", "name": "party" }),
            ],
            today,
        );
        assert!(!added.contains(&"Thanksgiving") && !added.contains(&"Christmas Eve"));
    }

    #[test]
    fn schedule_ids_must_be_uuids() {
        assert!(hours_id("../seats").is_err());
        assert!(hours_id("11111111-2222-3333-4444-555555555555").is_ok());
    }

    #[test]
    fn holiday_lines_read_naturally() {
        let l = holiday_line(&json!({ "date": "2026-12-25", "name": "Christmas Day", "recursAnnually": true, "observed": true, "closedAllDay": true }));
        assert_eq!(l, "2026-12-25 Christmas Day (every year, observed) closed all day");
        let l = holiday_line(&json!({ "date": "2026-12-24", "closedAllDay": false, "startMinute": 540, "endMinute": 720 }));
        assert_eq!(l, "2026-12-24 (unnamed) (once) open 09:00-12:00");
    }
}
