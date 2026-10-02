//! `th smoo notifications prefs …` — YOUR OWN notification routing in the
//! active org (SMOODEV-3610). CLI twin of the copilot's
//! `notifications.get_preferences` / `notifications.update_preferences` and the
//! hosted MCP tools of the same names.
//!
//! | verb  | route                                                    |
//! |-------|----------------------------------------------------------|
//! | `get` | `GET /organizations/{org}/notifications/preferences`     |
//! | `set` | `PUT /organizations/{org}/notifications/preferences`     |
//!
//! Preferences are keyed on (user, org), so this rides the signed-in USER
//! session ([`UserClient`]). An org API key has no person behind it — upstream
//! would resolve it to the org's first admin and edit THEIR routing — so there
//! is deliberately no M2M fallback: without `th auth login` the command refuses.
//!
//! The PUT keeps any field it is not sent, but REPLACES `categoryRouting` and
//! `quietHours` wholesale when they are present. So `set` reads the current
//! prefs, merges the change into those two objects client-side (preserving
//! keys it does not know, e.g. `quietHours.callsAlwaysRing`), validates, and
//! PUTs only the keys it touched. The server re-validates everything; the
//! local checks exist so a typo fails before the round-trip rather than
//! storing a setting dispatch would silently ignore.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::print_json;
use crate::destructive::{Confirm, Severity, Target};
use crate::smooai::user_client::UserClient;

pub const CHANNELS: &[&str] = &["in_app", "push", "email", "sms", "slack"];
pub const SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];

#[derive(Subcommand)]
pub enum Cmd {
    /// Your notification preferences (channels, per-category routing, quiet hours).
    #[command(subcommand)]
    Prefs(PrefsCmd),
}

#[derive(Subcommand)]
pub enum PrefsCmd {
    /// Show your current notification routing.
    Get {
        /// Print raw JSON instead of the summary.
        #[arg(long)]
        json: bool,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Change your notification routing. Only the flags you pass change.
    Set {
        /// Email channel: on | off.
        #[arg(long)]
        email: Option<String>,
        /// SMS channel: on | off.
        #[arg(long)]
        sms: Option<String>,
        /// In-app channel: on | off.
        #[arg(long)]
        in_app: Option<String>,
        /// Mobile push channel: on | off.
        #[arg(long)]
        push: Option<String>,
        /// Slack channel: on | off.
        #[arg(long)]
        slack: Option<String>,
        /// Route a category: `<category>=<ch1+ch2>[:<minSeverity>]`, e.g.
        /// `human_escalation=push+sms:high`. An empty channel list mutes it.
        /// Repeatable.
        #[arg(long = "route")]
        routes: Vec<String>,
        /// Remove a category's routing override (falls back to the channels above). Repeatable.
        #[arg(long = "clear-route")]
        clear_routes: Vec<String>,
        /// Quiet hours: on | off. Turning them on needs start, end and tz.
        #[arg(long)]
        quiet_hours: Option<String>,
        /// Quiet-hours start, 24h HH:MM.
        #[arg(long)]
        quiet_start: Option<String>,
        /// Quiet-hours end, 24h HH:MM.
        #[arg(long)]
        quiet_end: Option<String>,
        /// Quiet-hours IANA timezone, e.g. America/New_York.
        #[arg(long)]
        tz: Option<String>,
        /// Let critical alerts through quiet hours: on | off.
        #[arg(long)]
        allow_critical: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

/// Every `set` flag, parsed but not yet merged.
#[derive(Debug, Default, Clone)]
pub struct PrefsChange {
    pub email: Option<String>,
    pub sms: Option<String>,
    pub in_app: Option<String>,
    pub push: Option<String>,
    pub slack: Option<String>,
    pub routes: Vec<String>,
    pub clear_routes: Vec<String>,
    pub quiet_hours: Option<String>,
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
    pub tz: Option<String>,
    pub allow_critical: Option<String>,
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    let Cmd::Prefs(cmd) = cmd;
    let client = UserClient::from_user_session()
        .await
        .context("notification preferences are per person — sign in with `th auth login` (an org API key has no preferences of its own)")?;
    match cmd {
        PrefsCmd::Get { json, org } => {
            let org = crate::active_org::resolve(org)?;
            let body = client.get(&prefs_path(&org)).await.context("GET notifications/preferences")?;
            if json {
                print_json(&body);
            } else {
                println!();
                println!("{}", render(&body));
                println!();
            }
            Ok(())
        }
        PrefsCmd::Set {
            email,
            sms,
            in_app,
            push,
            slack,
            routes,
            clear_routes,
            quiet_hours,
            quiet_start,
            quiet_end,
            tz,
            allow_critical,
            confirm,
            org,
        } => {
            let org = crate::active_org::resolve(org)?;
            let change = PrefsChange {
                email,
                sms,
                in_app,
                push,
                slack,
                routes,
                clear_routes,
                quiet_hours,
                quiet_start,
                quiet_end,
                tz,
                allow_critical,
            };
            let current = client.get(&prefs_path(&org)).await.context("GET notifications/preferences")?;
            let body = build_put(&current, &change)?;
            let keys: Vec<&str> = body.as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default();
            let target = format!("your notification preferences ({})", keys.join(", "));
            let proceed = crate::destructive::gate_with(
                &Target {
                    verb: "update",
                    noun: "notification routing",
                    id: &target,
                    org: &org,
                    severity: Severity::Standard,
                },
                confirm,
            )?;
            if proceed {
                let updated = client.put(&prefs_path(&org), &body).await.context("PUT notifications/preferences")?;
                println!("  {} updated your notification preferences", "✓".green());
                println!("{}", render(&updated));
                println!();
            }
            Ok(())
        }
    }
}

fn prefs_path(org: &str) -> String {
    format!("/organizations/{org}/notifications/preferences")
}

/// `on`/`off` (also true/false, yes/no, 1/0).
fn on_off(flag: &str, v: &str) -> Result<bool> {
    match v.trim().to_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => bail!("--{flag} takes on | off, got \"{v}\""),
    }
}

fn validate_hhmm(flag: &str, t: &str) -> Result<()> {
    let ok = t.len() == 5
        && t.split_once(':')
            .and_then(|(h, m)| Some((h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)))
            .is_some_and(|(h, m)| h < 24 && m < 60);
    if ok {
        Ok(())
    } else {
        bail!("--{flag} must be 24h HH:MM, e.g. 22:00 (got \"{t}\")")
    }
}

/// Parse `category=ch1+ch2[:minSeverity]` into `(category, route object)`.
pub fn parse_route(spec: &str) -> Result<(String, Value)> {
    let (cat, rest) = spec
        .split_once('=')
        .with_context(|| format!("--route \"{spec}\" must look like <category>=<ch1+ch2>[:<minSeverity>]"))?;
    let cat = cat.trim();
    if cat.is_empty() || !cat.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!("--route category \"{cat}\" must be a notification type like human_escalation");
    }
    let (chans, sev) = match rest.split_once(':') {
        Some((c, s)) => (c, Some(s.trim())),
        None => (rest, None),
    };
    let channels: Vec<String> = chans.split('+').map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect();
    for c in &channels {
        if !CHANNELS.contains(&c.as_str()) {
            bail!("unknown channel \"{c}\" in --route — use one of: {}", CHANNELS.join(", "));
        }
    }
    let mut route = json!({ "channels": channels });
    if let Some(s) = sev {
        if !SEVERITIES.contains(&s) {
            bail!("unknown severity \"{s}\" in --route — use one of: {}", SEVERITIES.join(", "));
        }
        route["minSeverity"] = json!(s);
    }
    Ok((cat.to_string(), route))
}

/// Merge `change` over `current` and return the PUT body: only the keys this
/// call touches, with `categoryRouting` / `quietHours` as full merged objects.
pub fn build_put(current: &Value, change: &PrefsChange) -> Result<Value> {
    let mut out = Map::new();
    for (flag, key, v) in [
        ("email", "emailEnabled", &change.email),
        ("sms", "smsEnabled", &change.sms),
        ("in-app", "inAppEnabled", &change.in_app),
        ("push", "pushEnabled", &change.push),
        ("slack", "slackEnabled", &change.slack),
    ] {
        if let Some(v) = v {
            out.insert(key.into(), json!(on_off(flag, v)?));
        }
    }

    if !change.routes.is_empty() || !change.clear_routes.is_empty() {
        let mut routing = current.get("categoryRouting").and_then(Value::as_object).cloned().unwrap_or_default();
        for cat in &change.clear_routes {
            routing.remove(cat.trim());
        }
        for spec in &change.routes {
            let (cat, mut route) = parse_route(spec)?;
            // Keep the existing severity floor when only channels were given.
            if route.get("minSeverity").is_none() {
                let prev = routing.get(&cat).and_then(|r| r.get("minSeverity")).cloned().unwrap_or_else(|| json!("low"));
                route["minSeverity"] = prev;
            }
            routing.insert(cat, route);
        }
        out.insert("categoryRouting".into(), Value::Object(routing));
    }

    let quiet_touched =
        change.quiet_hours.is_some() || change.quiet_start.is_some() || change.quiet_end.is_some() || change.tz.is_some() || change.allow_critical.is_some();
    if quiet_touched {
        let mut q = current.get("quietHours").and_then(Value::as_object).cloned().unwrap_or_default();
        if let Some(v) = &change.quiet_hours {
            q.insert("enabled".into(), json!(on_off("quiet-hours", v)?));
        }
        if let Some(t) = &change.quiet_start {
            validate_hhmm("quiet-start", t)?;
            q.insert("start".into(), json!(t));
        }
        if let Some(t) = &change.quiet_end {
            validate_hhmm("quiet-end", t)?;
            q.insert("end".into(), json!(t));
        }
        if let Some(tz) = &change.tz {
            if tz.trim().is_empty() {
                bail!("--tz must not be empty");
            }
            q.insert("tz".into(), json!(tz.trim()));
        }
        if let Some(v) = &change.allow_critical {
            q.insert("allowCritical".into(), json!(on_off("allow-critical", v)?));
        }
        if q.get("enabled").and_then(Value::as_bool) == Some(true) {
            let s = |k: &str| q.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
            let missing: Vec<&str> = [("start", "--quiet-start"), ("end", "--quiet-end"), ("tz", "--tz")]
                .into_iter()
                .filter(|(k, _)| s(k).is_none())
                .map(|(_, f)| f)
                .collect();
            if !missing.is_empty() {
                bail!(
                    "quiet hours can't be on without {} — without all three the window never applies",
                    missing.join(" and ")
                );
            }
            if s("start") == s("end") {
                bail!("quiet-hours start and end are the same time — that is an empty window");
            }
        }
        out.insert("quietHours".into(), Value::Object(q));
    }

    if out.is_empty() {
        bail!("nothing to change — pass a channel flag, --route/--clear-route, or a quiet-hours flag");
    }
    Ok(Value::Object(out))
}

fn render(p: &Value) -> String {
    let b = |k: &str, d: bool| if p.get(k).and_then(Value::as_bool).unwrap_or(d) { "on" } else { "off" };
    let mut out = format!(
        "  {} in_app {}, push {}, email {}, sms {}, slack {}",
        "Channels:".bold(),
        b("inAppEnabled", true),
        b("pushEnabled", true),
        b("emailEnabled", true),
        b("smsEnabled", false),
        b("slackEnabled", false),
    );
    let routing = p.get("categoryRouting").and_then(Value::as_object).cloned().unwrap_or_default();
    if routing.is_empty() {
        out.push_str("\n  Category routing: none (every category uses the channels above)");
    } else {
        out.push_str("\n  Category routing:");
        for (cat, r) in &routing {
            let chans: Vec<&str> = r
                .get("channels")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let chans = if chans.is_empty() { "muted".to_string() } else { chans.join("+") };
            let sev = r.get("minSeverity").and_then(Value::as_str).unwrap_or("low");
            out.push_str(&format!("\n    {cat}: {chans} (min {sev})"));
        }
    }
    let q = p.get("quietHours");
    if q.and_then(|q| q.get("enabled")).and_then(Value::as_bool) == Some(true) {
        let g = |k: &str| q.and_then(|q| q.get(k)).and_then(Value::as_str).unwrap_or("?");
        let crit = q.and_then(|q| q.get("allowCritical")).and_then(Value::as_bool).unwrap_or(true);
        out.push_str(&format!(
            "\n  Quiet hours: ON {}–{} ({}); critical {}",
            g("start"),
            g("end"),
            g("tz"),
            if crit { "still gets through" } else { "muted too" }
        ));
    } else {
        out.push_str("\n  Quiet hours: off");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Value {
        json!({
            "emailEnabled": true,
            "categoryRouting": { "human_escalation": { "channels": ["push"], "minSeverity": "high" } },
            "quietHours": { "enabled": false, "start": "22:00", "end": "07:00", "tz": "America/New_York", "callsAlwaysRing": true },
        })
    }

    #[test]
    fn channel_flags_map_to_the_route_field_names() {
        let c = PrefsChange {
            sms: Some("on".into()),
            in_app: Some("off".into()),
            ..PrefsChange::default()
        };
        assert_eq!(build_put(&current(), &c).unwrap(), json!({ "smsEnabled": true, "inAppEnabled": false }));
        let bad = PrefsChange {
            email: Some("maybe".into()),
            ..PrefsChange::default()
        };
        assert!(build_put(&current(), &bad).is_err());
    }

    #[test]
    fn nothing_to_change_is_refused() {
        assert!(build_put(&current(), &PrefsChange::default()).is_err());
    }

    #[test]
    fn routes_merge_into_the_existing_object() {
        let c = PrefsChange {
            routes: vec!["conversation_ended=email".into(), "human_escalation=push+sms".into()],
            ..PrefsChange::default()
        };
        let body = build_put(&current(), &c).unwrap();
        let r = &body["categoryRouting"];
        // The new category defaults to `low`; the edited one keeps its floor.
        assert_eq!(r["conversation_ended"], json!({ "channels": ["email"], "minSeverity": "low" }));
        assert_eq!(r["human_escalation"], json!({ "channels": ["push", "sms"], "minSeverity": "high" }));
        assert!(body.get("quietHours").is_none(), "an untouched block is not sent");
    }

    #[test]
    fn clear_route_removes_only_that_category() {
        let c = PrefsChange {
            clear_routes: vec!["human_escalation".into()],
            ..PrefsChange::default()
        };
        assert_eq!(build_put(&current(), &c).unwrap(), json!({ "categoryRouting": {} }));
    }

    #[test]
    fn route_specs_are_validated() {
        assert_eq!(
            parse_route("x_y=push:critical").unwrap().1,
            json!({ "channels": ["push"], "minSeverity": "critical" })
        );
        // Empty channel list = muted.
        assert_eq!(parse_route("x=").unwrap().1, json!({ "channels": [] }));
        assert!(parse_route("x=fax").is_err());
        assert!(parse_route("x=push:urgent").is_err());
        assert!(parse_route("no-equals").is_err());
        assert!(parse_route("Bad Cat=push").is_err());
    }

    #[test]
    fn quiet_hours_merge_preserves_unknown_keys() {
        let c = PrefsChange {
            quiet_hours: Some("on".into()),
            ..PrefsChange::default()
        };
        let body = build_put(&current(), &c).unwrap();
        assert_eq!(body["quietHours"]["enabled"], true);
        assert_eq!(body["quietHours"]["start"], "22:00");
        assert_eq!(body["quietHours"]["callsAlwaysRing"], true);
    }

    #[test]
    fn enabling_quiet_hours_needs_a_complete_window() {
        let c = PrefsChange {
            quiet_hours: Some("on".into()),
            ..PrefsChange::default()
        };
        let err = build_put(&json!({}), &c).unwrap_err().to_string();
        assert!(err.contains("--quiet-start") && err.contains("--tz"), "{err}");

        let same = PrefsChange {
            quiet_hours: Some("on".into()),
            quiet_start: Some("22:00".into()),
            quiet_end: Some("22:00".into()),
            tz: Some("UTC".into()),
            ..PrefsChange::default()
        };
        assert!(build_put(&json!({}), &same).is_err());

        let bad_time = PrefsChange {
            quiet_start: Some("25:00".into()),
            ..PrefsChange::default()
        };
        assert!(build_put(&current(), &bad_time).is_err());

        // Turning them OFF never needs a window.
        let off = PrefsChange {
            quiet_hours: Some("off".into()),
            ..PrefsChange::default()
        };
        assert_eq!(build_put(&json!({}), &off).unwrap(), json!({ "quietHours": { "enabled": false } }));
    }

    #[test]
    fn render_handles_defaults_and_routes() {
        let s = render(&json!({}));
        assert!(s.contains("Quiet hours: off"));
        let s = render(&current());
        assert!(s.contains("human_escalation: push (min high)"));
    }
}
