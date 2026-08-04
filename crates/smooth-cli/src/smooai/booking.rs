//! `th booking …` — the org's Google-Calendar-backed booking page:
//! availability config, open slots, bookings, and manual busy blocks.
//!
//! All routes are top-level `/booking/*` on `api.smoo.ai` (not
//! `/organizations/{id}/…`). Authenticated with whatever session is on
//! disk (`th api login` / `th auth login`) — the config/blocks routes
//! want a user session; the slots route is public but works authed too.
//!
//! `config set` is a merge, not a replace: it GETs the current config,
//! overlays only the flags you passed, and PUTs the result — so an
//! unset flag never clobbers an existing value.

use anstream::println;
use anyhow::{bail, Context, Result};
use chrono::DateTime;
use chrono_tz::Tz;
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::{print_json, require_active_org, require_authed};

#[derive(Subcommand)]
pub enum Cmd {
    /// Booking availability config (get / set).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Named booking types (Calendly-style event types) — list / create / update / rm.
    #[command(visible_alias = "type")]
    Types {
        #[command(subcommand)]
        cmd: TypesCmd,
    },
    /// Open booking slots for a member, rendered in the config timezone.
    Slots {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Member email to book with. Defaults to the config's `memberEmail`.
        #[arg(long = "member-email")]
        member_email: Option<String>,
        /// Meeting length in minutes. Defaults to the config's first duration.
        #[arg(long)]
        duration: Option<u32>,
        /// Window start (ISO 8601, e.g. 2026-07-09T00:00:00Z).
        #[arg(long)]
        from: Option<String>,
        /// Window end (ISO 8601).
        #[arg(long)]
        to: Option<String>,
        /// Print the raw slots JSON instead of the rendered list.
        #[arg(long)]
        json: bool,
    },
    /// Bookings made against the org.
    Bookings {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Manual calendar blocks — busy time carved out of availability.
    #[command(visible_alias = "blocks")]
    Block {
        #[command(subcommand)]
        cmd: BlockCmd,
    },
    /// Connected Google calendars — primary + extra conflict accounts.
    #[command(visible_alias = "calendar")]
    Calendars {
        #[command(subcommand)]
        cmd: CalendarsCmd,
    },
    /// Print the public booking link for the org (uses the config's link
    /// handle/slug when set, otherwise the member email).
    Link {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Booking type slug — prints the typed link `/book/{org}/{member}/{type}`.
        #[arg(long = "type")]
        type_slug: Option<String>,
        /// Ad-hoc personal note — appended as `?m=<urlencoded>` for the pre-filled link variant.
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
// `Set` carries a dozen optional flags next to a one-field `Get`; the enum
// is built exactly once per CLI invocation, so the size delta is irrelevant.
#[allow(clippy::large_enum_variant)]
pub enum ConfigCmd {
    /// Show the current booking config.
    Get {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update the config — GETs the current one, overlays only the flags
    /// you pass, and PUTs the merged payload (unset flags are preserved).
    Set {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// IANA timezone (e.g. America/New_York).
        #[arg(long)]
        timezone: Option<String>,
        /// Availability weekdays, comma-separated (1=Mon … 7=Sun).
        #[arg(long, value_delimiter = ',')]
        days: Option<Vec<u8>>,
        /// Day start as minutes from midnight (mutually exclusive with --start).
        #[arg(long = "start-minute")]
        start_minute: Option<u32>,
        /// Day start as HH:MM (mutually exclusive with --start-minute).
        #[arg(long)]
        start: Option<String>,
        /// Day end as minutes from midnight (mutually exclusive with --end).
        #[arg(long = "end-minute")]
        end_minute: Option<u32>,
        /// Day end as HH:MM (mutually exclusive with --end-minute).
        #[arg(long)]
        end: Option<String>,
        /// Offered meeting lengths in minutes, comma-separated.
        #[arg(long, value_delimiter = ',')]
        durations: Option<Vec<u32>>,
        /// Buffer between meetings, in minutes.
        #[arg(long)]
        buffer: Option<u32>,
        /// Minimum notice before a booking, in minutes.
        #[arg(long = "min-notice")]
        min_notice: Option<u32>,
        /// How far ahead bookings are allowed, in days.
        #[arg(long = "window-days")]
        window_days: Option<u32>,
        /// Google Calendar id that bookings are written to.
        #[arg(long = "calendar-id")]
        calendar_id: Option<String>,
        /// Extra calendars to check for conflicts, comma-separated.
        #[arg(long = "conflict-calendars", value_delimiter = ',')]
        conflict_calendars: Option<Vec<String>>,
        /// Public display name on the booking page.
        #[arg(long = "display-name")]
        display_name: Option<String>,
        /// Public link handle (org-unique). Server validates `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`.
        #[arg(long)]
        slug: Option<String>,
        /// Default online-meeting provider for new bookings.
        #[arg(long, value_parser = ["none", "google_meet", "teams", "zoom"])]
        conferencing: Option<String>,
        /// Avatar image URL shown on the public booking page.
        #[arg(long = "avatar-url")]
        avatar_url: Option<String>,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum TypesCmd {
    /// List the org's booking types.
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Create a booking type.
    Create {
        /// Display name of the type.
        #[arg(long)]
        name: String,
        /// Public slug (lowercase letters, numbers, hyphens).
        #[arg(long)]
        slug: String,
        /// Offered meeting lengths in minutes, comma-separated (e.g. 30,45).
        #[arg(long, value_delimiter = ',', required = true)]
        durations: Vec<u32>,
        /// Personal note shown on the public typed page (max 500).
        #[arg(long)]
        note: Option<String>,
        /// Online-meeting override. Omit to inherit the config's choice.
        #[arg(long, value_parser = ["none", "google_meet", "teams", "zoom"])]
        conferencing: Option<String>,
        /// Window start (ISO 8601) — link opens at this time.
        #[arg(long = "window-start")]
        window_start: Option<String>,
        /// Window end (ISO 8601) — link closes at this time.
        #[arg(long = "window-end")]
        window_end: Option<String>,
        /// One-time link (maxBookings = 1).
        #[arg(long = "one-time")]
        one_time: bool,
        /// Share on the public org directory (scope = org) instead of personal.
        #[arg(long = "org-shared")]
        org_shared: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Update a booking type by id (all fields are replaced by the payload).
    Update {
        /// The type id from `th booking types list`.
        type_id: String,
        /// Display name of the type.
        #[arg(long)]
        name: String,
        /// Public slug (lowercase letters, numbers, hyphens).
        #[arg(long)]
        slug: String,
        /// Offered meeting lengths in minutes, comma-separated (e.g. 30,45).
        #[arg(long, value_delimiter = ',', required = true)]
        durations: Vec<u32>,
        /// Personal note shown on the public typed page (max 500).
        #[arg(long)]
        note: Option<String>,
        /// Online-meeting override. Omit to inherit the config's choice.
        #[arg(long, value_parser = ["none", "google_meet", "teams", "zoom"])]
        conferencing: Option<String>,
        /// Window start (ISO 8601) — link opens at this time.
        #[arg(long = "window-start")]
        window_start: Option<String>,
        /// Window end (ISO 8601) — link closes at this time.
        #[arg(long = "window-end")]
        window_end: Option<String>,
        /// One-time link (maxBookings = 1).
        #[arg(long = "one-time")]
        one_time: bool,
        /// Share on the public org directory (scope = org) instead of personal.
        #[arg(long = "org-shared")]
        org_shared: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a booking type by id.
    Rm {
        /// The type id from `th booking types list`.
        type_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum BlockCmd {
    /// Add a manual busy block.
    Add {
        /// Block start (ISO 8601).
        #[arg(long)]
        start: String,
        /// Block end (ISO 8601).
        #[arg(long)]
        end: String,
        /// Optional reason shown on the calendar event.
        #[arg(long)]
        reason: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// List manual busy blocks in a window.
    List {
        /// Window start (ISO 8601).
        #[arg(long)]
        from: Option<String>,
        /// Window end (ISO 8601).
        #[arg(long)]
        to: Option<String>,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a manual busy block by its calendar event id.
    Rm {
        /// The event id from `th booking block list`.
        event_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CalendarsCmd {
    /// List the org's connected Google calendars (primary + conflict).
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Start the Google OAuth flow to add a conflict calendar. Prints an
    /// authorization URL to open in the browser signed into that account.
    Connect {
        /// Permission tier requested from Google.
        #[arg(long, default_value = "read_only")]
        tier: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Remove a connected calendar by its integration id.
    Rm {
        /// The integration id from `th booking calendars list`.
        integration_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// The `--*`-provided config overlay, resolved from clap flags (HH:MM
/// already converted to minutes). Kept separate from clap so the merge
/// is unit-testable without building a `Cmd`.
#[derive(Default)]
struct ConfigOverlay {
    timezone: Option<String>,
    days: Option<Vec<u8>>,
    start_minute: Option<u32>,
    end_minute: Option<u32>,
    durations: Option<Vec<u32>>,
    buffer: Option<u32>,
    min_notice: Option<u32>,
    window_days: Option<u32>,
    calendar_id: Option<String>,
    conflict_calendars: Option<Vec<String>>,
    display_name: Option<String>,
    slug: Option<String>,
    conferencing: Option<String>,
    avatar_url: Option<String>,
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Config { cmd: ConfigCmd::Get { org } } => {
            let o = require_active_org(&client, org)?;
            print_json(&client.get(&format!("/booking/config/{o}")).await.context("GET booking config")?);
        }
        Cmd::Config {
            cmd:
                ConfigCmd::Set {
                    org,
                    timezone,
                    days,
                    start_minute,
                    start,
                    end_minute,
                    end,
                    durations,
                    buffer,
                    min_notice,
                    window_days,
                    calendar_id,
                    conflict_calendars,
                    display_name,
                    slug,
                    conferencing,
                    avatar_url,
                },
        } => {
            let o = require_active_org(&client, org)?;
            let overlay = ConfigOverlay {
                timezone,
                days,
                start_minute: resolve_minute("start", start_minute, start.as_deref())?,
                end_minute: resolve_minute("end", end_minute, end.as_deref())?,
                durations,
                buffer,
                min_notice,
                window_days,
                calendar_id,
                conflict_calendars,
                display_name,
                slug,
                conferencing,
                avatar_url,
            };
            let current = client.get(&format!("/booking/config/{o}")).await.context("GET booking config")?;
            let payload = build_payload(&current, &overlay);
            print_json(&client.put(&format!("/booking/config/{o}"), &payload).await.context("PUT booking config")?);
        }
        Cmd::Types { cmd: TypesCmd::List { org } } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/booking/types/{o}")).await.context("GET booking types")?;
            // Prefer the config's public handle over each type's member email
            // when building URLs — matches the `link` command's contract.
            let slug = client
                .get(&format!("/booking/config/{o}"))
                .await
                .ok()
                .and_then(|c| c.get("slug").and_then(|v| v.as_str()).map(str::to_string));
            render_types(&body, &o, slug.as_deref());
        }
        Cmd::Types {
            cmd:
                TypesCmd::Create {
                    name,
                    slug,
                    durations,
                    note,
                    conferencing,
                    window_start,
                    window_end,
                    one_time,
                    org_shared,
                    org,
                },
        } => {
            let o = require_active_org(&client, org)?;
            let payload = build_type_payload(&TypeInput {
                name,
                slug,
                durations,
                note,
                conferencing,
                window_start,
                window_end,
                one_time,
                org_shared,
            });
            print_json(&client.post(&format!("/booking/types/{o}"), Some(&payload)).await.context("POST booking type")?);
        }
        Cmd::Types {
            cmd:
                TypesCmd::Update {
                    type_id,
                    name,
                    slug,
                    durations,
                    note,
                    conferencing,
                    window_start,
                    window_end,
                    one_time,
                    org_shared,
                    org,
                },
        } => {
            let o = require_active_org(&client, org)?;
            let payload = build_type_payload(&TypeInput {
                name,
                slug,
                durations,
                note,
                conferencing,
                window_start,
                window_end,
                one_time,
                org_shared,
            });
            print_json(
                &client
                    .patch(&format!("/booking/types/{o}/{}", urlencoding::encode(&type_id)), &payload)
                    .await
                    .context("PATCH booking type")?,
            );
        }
        Cmd::Types {
            cmd: TypesCmd::Rm { type_id, org },
        } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/booking/types/{o}/{}", urlencoding::encode(&type_id)))
                    .await
                    .context("DELETE booking type")?,
            );
        }
        Cmd::Slots {
            org,
            member_email,
            duration,
            from,
            to,
            json,
        } => {
            let o = require_active_org(&client, org)?;
            let config = client.get(&format!("/booking/config/{o}")).await.context("GET booking config")?;
            let member = member_email
                .or_else(|| config.get("memberEmail").and_then(|v| v.as_str()).map(str::to_string))
                .context("no --member-email and config has no memberEmail")?;
            let duration = duration.or_else(|| {
                config
                    .get("durationsMinutes")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32)
            });
            let mut path = format!("/booking/google/{o}/{}/slots", urlencoding::encode(&member));
            let mut params: Vec<String> = Vec::new();
            if let Some(d) = duration {
                params.push(format!("durationMinutes={d}"));
            }
            if let Some(f) = &from {
                params.push(format!("from={}", urlencoding::encode(f)));
            }
            if let Some(t) = &to {
                params.push(format!("to={}", urlencoding::encode(t)));
            }
            if !params.is_empty() {
                path.push('?');
                path.push_str(&params.join("&"));
            }
            let body = client.get(&path).await.context("GET booking slots")?;
            if json {
                print_json(&body);
                return Ok(());
            }
            let tz_name = config.get("timezone").and_then(|v| v.as_str()).unwrap_or("UTC");
            render_slots(&body, tz_name);
        }
        Cmd::Bookings { org } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/booking/bookings/{o}")).await.context("GET bookings")?;
            // The `bookings` shape isn't fixed by the contract, so print the
            // array verbatim rather than guessing at columns.
            print_json(body.get("bookings").unwrap_or(&body));
        }
        Cmd::Block {
            cmd: BlockCmd::Add { start, end, reason, org },
        } => {
            let o = require_active_org(&client, org)?;
            let mut b = json!({ "start": start, "end": end });
            if let Some(r) = reason {
                b["reason"] = json!(r);
            }
            print_json(&client.post(&format!("/booking/blocks/{o}"), Some(&b)).await.context("POST block")?);
        }
        Cmd::Block {
            cmd: BlockCmd::List { from, to, org },
        } => {
            let o = require_active_org(&client, org)?;
            let mut path = format!("/booking/blocks/{o}");
            let mut params: Vec<String> = Vec::new();
            if let Some(f) = &from {
                params.push(format!("from={}", urlencoding::encode(f)));
            }
            if let Some(t) = &to {
                params.push(format!("to={}", urlencoding::encode(t)));
            }
            if !params.is_empty() {
                path.push('?');
                path.push_str(&params.join("&"));
            }
            let body = client.get(&path).await.context("GET blocks")?;
            render_blocks(&body);
        }
        Cmd::Block {
            cmd: BlockCmd::Rm { event_id, org },
        } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/booking/blocks/{o}/{}", urlencoding::encode(&event_id)))
                    .await
                    .context("DELETE block")?,
            );
        }
        Cmd::Calendars {
            cmd: CalendarsCmd::List { org },
        } => {
            let o = require_active_org(&client, org)?;
            let body = client.get(&format!("/booking/calendars/{o}")).await.context("GET calendars")?;
            render_calendars(&body);
        }
        Cmd::Calendars {
            cmd: CalendarsCmd::Connect { tier, org },
        } => {
            let o = require_active_org(&client, org)?;
            // Real route is /organizations/{org}/integrations/google/oauth/authorize?tier=…
            // `purpose=conflict` is per the booking contract; the authorize route
            // ignores it today (state only carries orgId+tier) — the conflict
            // distinction needs the platform side to thread it through.
            let path = format!(
                "/organizations/{o}/integrations/google/oauth/authorize?purpose=conflict&tier={}",
                urlencoding::encode(&tier)
            );
            let body = client.get(&path).await.context("GET google authorize url")?;
            let url = body.get("url").and_then(|v| v.as_str()).context("authorize response missing `url`")?;
            println!();
            println!("  {} Open this URL in the browser signed into the Google account", "→".cyan());
            println!("    you want to add as a conflict calendar:");
            println!();
            println!("    {url}");
            println!();
        }
        Cmd::Calendars {
            cmd: CalendarsCmd::Rm { integration_id, org },
        } => {
            let o = require_active_org(&client, org)?;
            print_json(
                &client
                    .delete(&format!("/booking/calendars/{o}/{}", urlencoding::encode(&integration_id)))
                    .await
                    .context("DELETE calendar")?,
            );
        }
        Cmd::Link { org, type_slug, note } => {
            let o = require_active_org(&client, org)?;
            let config = client.get(&format!("/booking/config/{o}")).await.context("GET booking config")?;
            // Prefer the org-unique link handle; fall back to the member email.
            let handle = config
                .get("slug")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| config.get("memberEmail").and_then(|v| v.as_str()))
                .context("config has no slug or memberEmail — connect a Google Calendar first")?;
            let url = build_public_url(&o, handle, type_slug.as_deref(), note.as_deref());
            println!();
            println!("  {} {}", "→".cyan(), url.bold());
            if config.get("connected").and_then(serde_json::Value::as_bool) == Some(false) {
                println!("  {} calendar not connected — the link won't work until it is", "⚠".yellow());
            }
            println!();
        }
    }
    Ok(())
}

/// Resolve a "day start/end" from the mutually-exclusive `--x-minute N`
/// and `--x HH:MM` flags. Errors if both are set. `label` is only for
/// the error message.
fn resolve_minute(label: &str, minute: Option<u32>, hhmm: Option<&str>) -> Result<Option<u32>> {
    match (minute, hhmm) {
        (Some(_), Some(_)) => bail!("pass only one of --{label}-minute or --{label}"),
        (Some(m), None) => Ok(Some(m)),
        (None, Some(s)) => Ok(Some(parse_hhmm(s)?)),
        (None, None) => Ok(None),
    }
}

/// "HH:MM" → minutes from midnight. Rejects out-of-range and malformed input.
fn parse_hhmm(s: &str) -> Result<u32> {
    let (h, m) = s.split_once(':').with_context(|| format!("expected HH:MM, got {s:?}"))?;
    let h: u32 = h.parse().with_context(|| format!("bad hour in {s:?}"))?;
    let m: u32 = m.parse().with_context(|| format!("bad minute in {s:?}"))?;
    if h > 23 || m > 59 {
        bail!("time out of range in {s:?} (need 00:00–23:59)");
    }
    Ok(h * 60 + m)
}

/// Merge the current config with the provided overlay into a PUT body.
/// Starts from the current config, drops the read-only `memberEmail` /
/// `connected` fields, then sets only the keys the overlay supplies — so
/// unset flags keep their current values.
fn build_payload(current: &Value, overlay: &ConfigOverlay) -> Value {
    let mut obj = current.as_object().cloned().unwrap_or_default();
    obj.remove("memberEmail");
    obj.remove("connected");
    if let Some(v) = &overlay.timezone {
        obj.insert("timezone".into(), json!(v));
    }
    if let Some(v) = &overlay.days {
        obj.insert("availabilityDays".into(), json!(v));
    }
    if let Some(v) = overlay.start_minute {
        obj.insert("startMinute".into(), json!(v));
    }
    if let Some(v) = overlay.end_minute {
        obj.insert("endMinute".into(), json!(v));
    }
    if let Some(v) = &overlay.durations {
        obj.insert("durationsMinutes".into(), json!(v));
    }
    if let Some(v) = overlay.buffer {
        obj.insert("bufferMinutes".into(), json!(v));
    }
    if let Some(v) = overlay.min_notice {
        obj.insert("minNoticeMinutes".into(), json!(v));
    }
    if let Some(v) = overlay.window_days {
        obj.insert("bookingWindowDays".into(), json!(v));
    }
    if let Some(v) = &overlay.calendar_id {
        obj.insert("calendarId".into(), json!(v));
    }
    if let Some(v) = &overlay.conflict_calendars {
        obj.insert("conflictCalendarIds".into(), json!(v));
    }
    if let Some(v) = &overlay.display_name {
        obj.insert("displayName".into(), json!(v));
    }
    if let Some(v) = &overlay.slug {
        obj.insert("slug".into(), json!(v));
    }
    if let Some(v) = &overlay.conferencing {
        obj.insert("conferencing".into(), json!(v));
    }
    if let Some(v) = &overlay.avatar_url {
        obj.insert("avatarUrl".into(), json!(v));
    }
    Value::Object(obj)
}

/// Resolved `types create/update` flags, shared by both paths so the
/// payload shaping (durations, one-time → maxBookings, scope) lives in
/// one place and is unit-testable without a `Cmd`.
struct TypeInput {
    name: String,
    slug: String,
    durations: Vec<u32>,
    note: Option<String>,
    conferencing: Option<String>,
    window_start: Option<String>,
    window_end: Option<String>,
    one_time: bool,
    org_shared: bool,
}

/// Build the POST/PATCH body for a booking type. Optional windows,
/// conferencing, and note are only set when supplied (omitted = inherit /
/// unset server-side); `--one-time` maps to `maxBookings: 1` and
/// `--org-shared` to `scope: "org"` (default "personal").
fn build_type_payload(input: &TypeInput) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), json!(input.name));
    obj.insert("slug".into(), json!(input.slug));
    obj.insert("durationsMinutes".into(), json!(input.durations));
    obj.insert("scope".into(), json!(if input.org_shared { "org" } else { "personal" }));
    if input.one_time {
        obj.insert("maxBookings".into(), json!(1));
    }
    if let Some(v) = &input.note {
        obj.insert("note".into(), json!(v));
    }
    if let Some(v) = &input.conferencing {
        obj.insert("conferencing".into(), json!(v));
    }
    if let Some(v) = &input.window_start {
        obj.insert("windowStartAt".into(), json!(v));
    }
    if let Some(v) = &input.window_end {
        obj.insert("windowEndAt".into(), json!(v));
    }
    Value::Object(obj)
}

/// Public booking URL. `handle` is the config link slug or the member
/// email; `type_slug` appends the typed path; `note` appends `?m=<enc>`.
fn build_public_url(org: &str, handle: &str, type_slug: Option<&str>, note: Option<&str>) -> String {
    let mut url = format!("https://smoo.ai/book/{org}/{}", urlencoding::encode(handle));
    if let Some(t) = type_slug {
        url.push('/');
        url.push_str(&urlencoding::encode(t));
    }
    if let Some(n) = note {
        url.push_str(&format!("?m={}", urlencoding::encode(n)));
    }
    url
}

/// Render a slot ISO timestamp in `tz` as e.g. "Thu Jul 09 1:00 PM".
/// `None` if the string isn't a valid RFC 3339 timestamp.
fn format_slot_in_tz(iso: &str, tz: Tz) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?;
    Some(dt.with_timezone(&tz).format("%a %b %d %-I:%M %p").to_string())
}

/// Print the `{ slots: [ISO…] }` body as a human-readable list in `tz_name`.
fn render_slots(body: &Value, tz_name: &str) {
    let tz: Tz = tz_name.parse().unwrap_or(chrono_tz::UTC);
    let slots = body.get("slots").and_then(|v| v.as_array());
    println!();
    let Some(slots) = slots else {
        print_json(body);
        return;
    };
    if slots.is_empty() {
        println!("  {} {}", "●".dimmed(), "no open slots".dimmed());
        println!();
        return;
    }
    println!("  {} ({})", "Open slots".bold(), tz_name.dimmed());
    for slot in slots {
        let Some(iso) = slot.as_str() else { continue };
        match format_slot_in_tz(iso, tz) {
            Some(human) => println!("  {} {}", "○".dimmed(), human),
            None => println!("  {} {}", "○".dimmed(), iso.dimmed()),
        }
    }
    println!();
}

/// Print the `{ blocks: [{eventId, summary, start, end}] }` body as a list.
fn render_blocks(body: &Value) {
    let blocks = body.get("blocks").and_then(|v| v.as_array());
    println!();
    let Some(blocks) = blocks else {
        print_json(body);
        return;
    };
    if blocks.is_empty() {
        println!("  {} {}", "●".dimmed(), "no blocks".dimmed());
        println!();
        return;
    }
    for b in blocks {
        let id = b.get("eventId").and_then(|v| v.as_str()).unwrap_or("?");
        let summary = b.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let start = b.get("start").and_then(|v| v.as_str()).unwrap_or("");
        let end = b.get("end").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {} {} {}  {}–{}", "○".dimmed(), id.cyan(), summary.bold(), start.dimmed(), end.dimmed());
    }
    println!();
}

/// Print the `{ calendars: [{integrationId, userEmail, role, status}] }` body as a list.
fn render_calendars(body: &Value) {
    let calendars = body.get("calendars").and_then(|v| v.as_array());
    println!();
    let Some(calendars) = calendars else {
        print_json(body);
        return;
    };
    if calendars.is_empty() {
        println!("  {} {}", "●".dimmed(), "no connected calendars".dimmed());
        println!();
        return;
    }
    for c in calendars {
        let id = c.get("integrationId").and_then(|v| v.as_str()).unwrap_or("?");
        let email = c.get("userEmail").and_then(|v| v.as_str()).unwrap_or("");
        let role = c.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let suffix = if status.is_empty() { String::new() } else { format!(" [{status}]") };
        println!("  {} {} {}  {}{}", "○".dimmed(), id.cyan(), email.bold(), role.dimmed(), suffix.dimmed());
    }
    println!();
}

/// Print the `{ types: [...] }` body as a list: name, slug, durations,
/// note (truncated), scope, window, and the full public URL per type.
fn render_types(body: &Value, org: &str, config_slug: Option<&str>) {
    let types = body.get("types").and_then(|v| v.as_array());
    println!();
    let Some(types) = types else {
        print_json(body);
        return;
    };
    if types.is_empty() {
        println!("  {} {}", "●".dimmed(), "no booking types".dimmed());
        println!();
        return;
    }
    for t in types {
        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let slug = t.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        let scope = t.get("scope").and_then(|v| v.as_str()).unwrap_or("personal");
        let durations: Vec<String> = t
            .get("durationsMinutes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(serde_json::Value::as_u64).map(|n| format!("{n}m")).collect())
            .unwrap_or_default();
        // Build the public URL with the config handle, falling back to the type's member email.
        let handle = config_slug
            .filter(|s| !s.is_empty())
            .or_else(|| t.get("memberEmail").and_then(|v| v.as_str()))
            .unwrap_or("");
        let url = build_public_url(org, handle, Some(slug), None);
        println!("  {} {} {}  {}", "○".dimmed(), slug.cyan(), name.bold(), durations.join(",").dimmed());
        println!("    {} {}", "scope".dimmed(), scope.dimmed());
        if let Some(note) = t.get("note").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            println!("    {} {}", "note ".dimmed(), truncate(note, 60).dimmed());
        }
        let start = t.get("windowStartAt").and_then(|v| v.as_str());
        let end = t.get("windowEndAt").and_then(|v| v.as_str());
        if start.is_some() || end.is_some() {
            println!("    {} {}–{}", "win  ".dimmed(), start.unwrap_or("").dimmed(), end.unwrap_or("").dimmed());
        }
        println!("    {} {}", "url  ".dimmed(), url.dimmed());
    }
    println!();
}

/// Truncate `s` to `max` chars, appending `…` when clipped. Char-based so
/// it never splits a multibyte boundary.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_parses_to_minutes() {
        assert_eq!(parse_hhmm("00:00").unwrap(), 0);
        assert_eq!(parse_hhmm("09:00").unwrap(), 540);
        assert_eq!(parse_hhmm("13:30").unwrap(), 810);
        assert_eq!(parse_hhmm("23:59").unwrap(), 1439);
    }

    #[test]
    fn hhmm_rejects_bad_input() {
        assert!(parse_hhmm("24:00").is_err()); // hour out of range
        assert!(parse_hhmm("12:60").is_err()); // minute out of range
        assert!(parse_hhmm("900").is_err()); // no colon
        assert!(parse_hhmm("9:aa").is_err()); // non-numeric minute
        assert!(parse_hhmm("").is_err());
    }

    #[test]
    fn resolve_minute_is_mutually_exclusive() {
        assert_eq!(resolve_minute("start", None, None).unwrap(), None);
        assert_eq!(resolve_minute("start", Some(540), None).unwrap(), Some(540));
        assert_eq!(resolve_minute("start", None, Some("09:00")).unwrap(), Some(540));
        assert!(resolve_minute("start", Some(540), Some("09:00")).is_err());
    }

    #[test]
    fn merge_overlays_only_provided_flags() {
        let current = json!({
            "timezone": "America/New_York",
            "availabilityDays": [1, 2, 3, 4, 5],
            "startMinute": 540,
            "endMinute": 1020,
            "durationsMinutes": [30, 60],
            "bufferMinutes": 15,
            "minNoticeMinutes": 120,
            "bookingWindowDays": 30,
            "calendarId": "primary",
            "conflictCalendarIds": ["a@x.com"],
            "displayName": "Old Name",
            "avatarUrl": "https://img/x.png",
            "memberEmail": "me@x.com",
            "connected": true,
        });
        let overlay = ConfigOverlay {
            display_name: Some("New Name".into()),
            buffer: Some(0),
            ..Default::default()
        };
        let out = build_payload(&current, &overlay);

        // Overridden fields take the new value.
        assert_eq!(out["displayName"], json!("New Name"));
        assert_eq!(out["bufferMinutes"], json!(0));
        // Untouched fields are preserved, including ones with no flag (avatarUrl).
        assert_eq!(out["timezone"], json!("America/New_York"));
        assert_eq!(out["durationsMinutes"], json!([30, 60]));
        assert_eq!(out["avatarUrl"], json!("https://img/x.png"));
        // Read-only fields are stripped from the PUT payload.
        assert!(out.get("memberEmail").is_none());
        assert!(out.get("connected").is_none());
    }

    #[test]
    fn merge_from_empty_config_still_sets_flags() {
        let out = build_payload(
            &json!({}),
            &ConfigOverlay {
                timezone: Some("UTC".into()),
                days: Some(vec![1, 2, 3]),
                start_minute: Some(480),
                durations: Some(vec![60, 90]),
                conflict_calendars: Some(vec!["b@y.com".into()]),
                ..Default::default()
            },
        );
        assert_eq!(out["timezone"], json!("UTC"));
        assert_eq!(out["availabilityDays"], json!([1, 2, 3]));
        assert_eq!(out["startMinute"], json!(480));
        assert_eq!(out["durationsMinutes"], json!([60, 90]));
        assert_eq!(out["conflictCalendarIds"], json!(["b@y.com"]));
    }

    #[test]
    fn slot_renders_in_config_timezone() {
        // 17:00 UTC is 13:00 in America/New_York (EDT, -4) on this date.
        let s = format_slot_in_tz("2026-07-09T17:00:00Z", chrono_tz::America::New_York).unwrap();
        assert_eq!(s, "Thu Jul 09 1:00 PM");
    }

    #[test]
    fn slot_renders_from_offset_string() {
        // Offset already baked into the string → still converted to tz.
        let s = format_slot_in_tz("2026-07-09T13:00:00-04:00", chrono_tz::America::New_York).unwrap();
        assert_eq!(s, "Thu Jul 09 1:00 PM");
    }

    #[test]
    fn slot_bad_timestamp_is_none() {
        assert!(format_slot_in_tz("not-a-date", chrono_tz::UTC).is_none());
    }

    #[test]
    fn config_set_writes_slug_conferencing_avatar() {
        let out = build_payload(
            &json!({}),
            &ConfigOverlay {
                slug: Some("brent".into()),
                conferencing: Some("google_meet".into()),
                avatar_url: Some("https://img/a.png".into()),
                ..Default::default()
            },
        );
        assert_eq!(out["slug"], json!("brent"));
        assert_eq!(out["conferencing"], json!("google_meet"));
        assert_eq!(out["avatarUrl"], json!("https://img/a.png"));
    }

    #[test]
    fn type_payload_minimal() {
        let out = build_type_payload(&TypeInput {
            name: "Intro".into(),
            slug: "intro".into(),
            durations: vec![30],
            note: None,
            conferencing: None,
            window_start: None,
            window_end: None,
            one_time: false,
            org_shared: false,
        });
        assert_eq!(out["name"], json!("Intro"));
        assert_eq!(out["slug"], json!("intro"));
        assert_eq!(out["durationsMinutes"], json!([30]));
        assert_eq!(out["scope"], json!("personal"));
        // Optional fields stay absent when unset (server inherits / leaves null).
        assert!(out.get("maxBookings").is_none());
        assert!(out.get("note").is_none());
        assert!(out.get("conferencing").is_none());
        assert!(out.get("windowStartAt").is_none());
    }

    #[test]
    fn type_payload_full() {
        let out = build_type_payload(&TypeInput {
            name: "Demo".into(),
            slug: "demo".into(),
            durations: vec![30, 45],
            note: Some("bring your team".into()),
            conferencing: Some("zoom".into()),
            window_start: Some("2026-07-09T00:00:00Z".into()),
            window_end: Some("2026-07-16T00:00:00Z".into()),
            one_time: true,
            org_shared: true,
        });
        assert_eq!(out["durationsMinutes"], json!([30, 45]));
        assert_eq!(out["scope"], json!("org"));
        assert_eq!(out["maxBookings"], json!(1));
        assert_eq!(out["note"], json!("bring your team"));
        assert_eq!(out["conferencing"], json!("zoom"));
        assert_eq!(out["windowStartAt"], json!("2026-07-09T00:00:00Z"));
        assert_eq!(out["windowEndAt"], json!("2026-07-16T00:00:00Z"));
    }

    #[test]
    fn public_url_variants() {
        assert_eq!(build_public_url("org1", "brent", None, None), "https://smoo.ai/book/org1/brent");
        assert_eq!(build_public_url("org1", "brent", Some("demo"), None), "https://smoo.ai/book/org1/brent/demo");
        // Note is url-encoded into the `?m=` param.
        assert_eq!(
            build_public_url("org1", "brent", Some("demo"), Some("hi there")),
            "https://smoo.ai/book/org1/brent/demo?m=hi%20there"
        );
        // Email handle is url-encoded (the `@` survives, the fallback path works).
        assert_eq!(build_public_url("org1", "me@x.com", None, None), "https://smoo.ai/book/org1/me%40x.com");
    }

    #[test]
    fn truncate_clips_with_ellipsis() {
        assert_eq!(truncate("short", 60), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // Char-based: multibyte input isn't split mid-codepoint.
        assert_eq!(truncate("héllo wörld", 4), "héll…");
    }
}
