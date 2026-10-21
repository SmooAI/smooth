//! `th smoo calling ring-groups list|show|set|members|create|delete` — ring
//! groups (phone "departments"), the CLI twin of the hosted MCP's
//! `ring_groups_list` / `ring_groups_get` / `ring_groups_update` /
//! `ring_group_members_set` / `ring_groups_create` / `ring_groups_delete` and
//! the copilot's `ring_groups.*` (SMOODEV-3960, create/delete SMOODEV-4052).
//!
//! Every verb calls the same api-prime route the Calling settings page calls
//! (`/organizations/{org}/calling/ring-groups…`) with your own sign-in, so the
//! route decides: `calling.use` to read, `calling.admin` to write, plus the
//! `telephony` product. A 403 is reported as "admin only".
//!
//! Validation mirrors smooai's shared validator
//! (`rust/smooth-operator-agent/src/tools_ring_groups.rs`): strategy is
//! simultaneous / sequential / `round_robin`, `--ring-first-seconds` is 5-120,
//! `--notify-email` is ONE address (`""` clears it), at most 100 unique members.
//! The org-wide `callDepartmentRoutingEnabled` switch is a config flag, not a
//! ring group setting, and no verb here can change it.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use std::fmt::Write as _;

use serde_json::{json, Map, Value};

use smooth_api_client::SmoothApiClient;

use super::{print_json, read_body, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// `RING_STRATEGIES` in smooai api-prime `handlers/calling/values.rs`.
pub const STRATEGIES: &[&str] = &["simultaneous", "sequential", "round_robin"];
/// `RING_FIRST_RANGE` in api-prime `handlers/calling/ring_groups.rs`.
pub const RING_FIRST_SECONDS: (i64, i64) = (5, 120);
/// `MAX_NOTIFY_EMAIL` in api-prime.
pub const MAX_NOTIFY_EMAIL: usize = 254;
/// `MAX_MEMBERS` in api-prime.
pub const MAX_MEMBERS: usize = 100;
/// `MAX_NAME` in api-prime `handlers/calling/ring_groups.rs`.
pub const MAX_NAME: usize = 100;

#[derive(Subcommand)]
pub enum RingGroupsCmd {
    /// The org's ring groups: strategy, ring-first seconds, message-only, enabled.
    #[command(visible_alias = "ls")]
    List {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// One ring group with its members in ring order.
    #[command(visible_alias = "get")]
    Show {
        /// The ring group id (from `calling ring-groups list`).
        group_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a ring group's settings (needs calling admin). Only what you pass changes.
    #[command(visible_alias = "update")]
    Set {
        /// The ring group id (from `calling ring-groups list`).
        group_id: String,
        #[command(flatten)]
        settings: Settings,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Replace a ring group's member list (needs calling admin). The list is the
    /// COMPLETE ring order: anyone left out is removed. E.g.
    /// `--member <userId>:first --member <userId> --member <userId>:off`.
    Members {
        /// The ring group id (from `calling ring-groups list`).
        group_id: String,
        /// A member, in ring order (repeatable): `<userId>`, with `:first` to ring
        /// first and/or `:off` to keep them listed but not rung.
        #[arg(long = "member", value_name = "USER_ID[:first][:off]")]
        members: Vec<String>,
        /// The full list as JSON from a file (`-` for stdin):
        /// `[{"userId": "…", "ringFirst": true, "enabled": true}, …]` or `{"members": [...]}`.
        #[arg(long, conflicts_with_all = ["members", "clear"])]
        body: Option<String>,
        /// Empty the group (it then rings nobody).
        #[arg(long, conflicts_with = "members")]
        clear: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Create a ring group (needs calling admin). It starts with no members and
    /// no number routes to it: add people with `members`, then point a number at
    /// it with `calling numbers update <id> --set routeType=ring_group --set routeRingGroupId=<id>`.
    #[command(visible_alias = "new")]
    Create {
        /// The department's name, unique in the org (1-100 characters), e.g. "Billing".
        name: String,
        #[command(flatten)]
        settings: Settings,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Delete a ring group and its member list (needs calling admin). Refused
    /// while a phone number routes to it; reroute that number first.
    #[command(visible_alias = "rm")]
    Delete {
        /// The ring group id (from `calling ring-groups list`).
        group_id: String,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then the credentials file's `active_org_id`.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// The settings `set` can change.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct Settings {
    /// simultaneous (everyone at once), sequential (in member order) or `round_robin`.
    #[arg(long)]
    pub strategy: Option<String>,
    /// 5-120: how long the ring-first members ring alone before everyone else.
    #[arg(long = "ring-first-seconds")]
    pub ring_first_seconds: Option<i64>,
    /// true = never rings; the phone AI takes a message and emails it to the notify email.
    #[arg(long = "message-only")]
    pub message_only: Option<bool>,
    /// ONE address message-only messages are emailed to; `""` clears it.
    #[arg(long = "notify-email")]
    pub notify_email: Option<String>,
    /// false takes the group out of routing.
    #[arg(long)]
    pub enabled: Option<bool>,
}

/// A ring group id: a UUID, so it can never re-point the request at another route.
pub fn group_id(id: &str) -> Result<String> {
    let id = id.trim();
    let ok = id.len() == 36
        && id
            .chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    if !ok {
        bail!("ring group id must be a uuid (from `th smoo calling ring-groups list`); got {id:?}");
    }
    Ok(id.to_ascii_lowercase())
}

/// One email address — the same rule as api-prime's `looks_like_one_email`.
#[must_use]
pub fn looks_like_one_email(s: &str) -> bool {
    if s.chars().any(char::is_whitespace) || s.contains(',') || s.contains(';') {
        return false;
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains("..")
}

/// The PATCH body for `set`.
pub fn settings_body(s: &Settings) -> Result<Value> {
    let mut body = Map::new();
    if let Some(v) = &s.strategy {
        let v = v.trim();
        if !STRATEGIES.contains(&v) {
            bail!("--strategy must be one of {}", STRATEGIES.join(", "));
        }
        body.insert("strategy".into(), json!(v));
    }
    if let Some(n) = s.ring_first_seconds {
        let (lo, hi) = RING_FIRST_SECONDS;
        if !(lo..=hi).contains(&n) {
            bail!("--ring-first-seconds must be from {lo} to {hi}");
        }
        body.insert("ringFirstSeconds".into(), json!(n));
    }
    if let Some(b) = s.message_only {
        body.insert("messageOnly".into(), json!(b));
    }
    if let Some(e) = &s.notify_email {
        let e = e.trim();
        if e.is_empty() {
            body.insert("notifyEmail".into(), Value::Null);
        } else if e.chars().count() > MAX_NOTIFY_EMAIL || !looks_like_one_email(e) {
            bail!("--notify-email must be a single email address (one address, no lists); got {e:?}");
        } else {
            body.insert("notifyEmail".into(), json!(e));
        }
    }
    if let Some(b) = s.enabled {
        body.insert("enabled".into(), json!(b));
    }
    if body.is_empty() {
        bail!("nothing to change — pass --strategy, --ring-first-seconds, --message-only, --notify-email or --enabled");
    }
    Ok(Value::Object(body))
}

/// The POST body for `create`: the name plus whichever settings were given.
pub fn create_body(name: &str, s: &Settings) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        bail!("the name must be 1 to {MAX_NAME} characters");
    }
    let mut body = if *s == Settings::default() {
        Map::new()
    } else {
        match settings_body(s)? {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    };
    // A null notify email on a new group is just "none".
    body.retain(|_, v| !v.is_null());
    body.insert("name".into(), json!(name));
    Ok(Value::Object(body))
}

/// `<userId>[:first][:off]` as one member entry.
pub fn parse_member(spec: &str) -> Result<Value> {
    let mut parts = spec.split(':');
    let id = group_id(parts.next().unwrap_or_default()).map_err(|_| anyhow::anyhow!("--member {spec:?}: the user id must be a uuid"))?;
    let mut entry = Map::new();
    entry.insert("userId".into(), json!(id));
    for flag in parts {
        match flag.trim() {
            "first" => entry.insert("ringFirst".into(), json!(true)),
            "off" | "disabled" => entry.insert("enabled".into(), json!(false)),
            other => bail!("--member {spec:?}: unknown flag {other:?} (use :first and/or :off)"),
        };
    }
    Ok(Value::Object(entry))
}

/// The PUT body: `{"members": [...]}`, validated (shape, uuids, no duplicates, ≤ 100).
pub fn members_body(list: Vec<Value>) -> Result<Value> {
    if list.len() > MAX_MEMBERS {
        bail!("a ring group can have at most {MAX_MEMBERS} members");
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for (i, m) in list.into_iter().enumerate() {
        let Value::Object(m) = m else {
            bail!("members[{i}] must be an object with a userId");
        };
        if let Some(k) = m.keys().find(|k| !matches!(k.as_str(), "userId" | "ringFirst" | "enabled")) {
            bail!("members[{i}].{k} is not a member field — use userId, ringFirst, enabled (list order is ring order)");
        }
        let id = group_id(m.get("userId").and_then(Value::as_str).unwrap_or_default()).map_err(|_| anyhow::anyhow!("members[{i}].userId must be a uuid"))?;
        if !seen.insert(id.clone()) {
            bail!("members[{i}].userId {id} appears more than once");
        }
        for k in ["ringFirst", "enabled"] {
            if m.get(k).is_some_and(|v| !v.is_boolean() && !v.is_null()) {
                bail!("members[{i}].{k} must be true or false");
            }
        }
        let mut entry = m;
        entry.insert("userId".into(), json!(id));
        entry.retain(|_, v| !v.is_null());
        out.push(Value::Object(entry));
    }
    Ok(json!({ "members": out }))
}

/// The member list from `--member` specs, a `--body` file, or `--clear`.
fn member_list(members: &[String], body: Option<&str>, clear: bool) -> Result<Vec<Value>> {
    const SHAPE: &str = "--body must be a JSON array of members or {\"members\": [...]}";
    match body {
        Some(path) => match read_body(path)? {
            Value::Array(a) => Ok(a),
            Value::Object(mut m) => match m.remove("members") {
                Some(Value::Array(a)) if m.is_empty() => Ok(a),
                _ => bail!(SHAPE),
            },
            _ => bail!(SHAPE),
        },
        None if clear => Ok(Vec::new()),
        None if members.is_empty() => bail!("pass the full list with --member (repeatable), --body, or --clear to empty the group"),
        None => members.iter().map(|m| parse_member(m)).collect(),
    }
}

/// A 403 from the route reads as "admin only", not an outage.
pub(super) fn admin_only(err: anyhow::Error, write: bool) -> anyhow::Error {
    if err.to_string().contains("HTTP 403") {
        let need = if write {
            "admin only: changing ring groups needs the calling admin permission (calling.admin)"
        } else {
            "reading ring groups needs calling access (calling.use)"
        };
        return err.context(format!("{need} — nothing was changed"));
    }
    err
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// One group's settings as a line.
#[must_use]
pub fn group_line(g: &Value) -> String {
    let mut out = format!("{} ({})", s(g, "name"), s(g, "id"));
    if g.get("enabled").and_then(Value::as_bool) == Some(false) {
        out.push_str(" [disabled]");
    }
    if g.get("messageOnly").and_then(Value::as_bool) == Some(true) {
        match s(g, "notifyEmail") {
            "" => out.push_str(" — message-only, NO notify email"),
            e => {
                let _ = write!(out, " — message-only → {e}");
            }
        }
    } else {
        let _ = write!(out, " — {}", s(g, "strategy"));
        if let Some(n) = g.get("ringFirstSeconds").and_then(Value::as_i64) {
            let _ = write!(out, ", ring-first {n}s");
        }
    }
    if let Some(n) = g.get("memberCount").and_then(Value::as_i64) {
        let _ = write!(out, ", {n} member(s)");
    }
    out
}

fn render_detail(g: &Value) {
    println!("\n  {}", group_line(g).bold());
    let members = g.get("members").and_then(Value::as_array).cloned().unwrap_or_default();
    if members.is_empty() {
        println!("    {}", "no members — rings nobody".dimmed());
    }
    for (i, m) in members.iter().enumerate() {
        let mut flags = Vec::new();
        if m.get("ringFirst").and_then(Value::as_bool) == Some(true) {
            flags.push("rings first");
        }
        if m.get("enabled").and_then(Value::as_bool) == Some(false) {
            flags.push("off");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" ({})", flags.join(", "))
        };
        println!("    {}. {}{}", i + 1, s(m, "userId"), flags);
    }
    println!();
}

/// One confirmed write to a ring group route.
struct Change<'a> {
    method: &'a str,
    path: String,
    body: Value,
    verb: &'a str,
    id: &'a str,
    org: &'a str,
}

async fn apply(client: &SmoothApiClient, change: Change<'_>, confirm: Confirm) -> Result<()> {
    let Change {
        method,
        path,
        body,
        verb,
        id,
        org,
    } = change;
    let path = path.as_str();
    if confirm.dry_run {
        println!("\n  {} would {method} {path}", "●".dimmed());
        print_json(&body);
    }
    let proceed = crate::destructive::gate_with(
        &Target {
            verb,
            noun: "ring group",
            id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )?;
    if proceed {
        let result = if method == "PUT" {
            client.put(path, &body).await
        } else {
            client.patch(path, &body).await
        };
        let updated = result.map_err(|e| admin_only(e, true)).with_context(|| format!("{method} {path}"))?;
        println!("\n  {} updated", "✓".green());
        render_detail(&updated);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb
pub async fn ring_groups(cmd: RingGroupsCmd) -> Result<()> {
    match cmd {
        RingGroupsCmd::List { org, json } => {
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/ring-groups?limit=200"))
                .await
                .map_err(|e| admin_only(e, false))
                .context("GET calling/ring-groups")?;
            if json {
                print_json(&body);
                return Ok(());
            }
            let list = body.as_array().cloned().unwrap_or_default();
            println!();
            if list.is_empty() {
                println!("  {} {}", "●".dimmed(), "no ring groups".dimmed());
            }
            for g in &list {
                println!("  {} {}", "○".dimmed(), group_line(g));
            }
            println!();
        }
        RingGroupsCmd::Show { group_id: id, org, json } => {
            let id = group_id(&id)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let body = client
                .get(&format!("/organizations/{o}/calling/ring-groups/{id}"))
                .await
                .map_err(|e| admin_only(e, false))
                .context("GET calling/ring-groups/:id")?;
            if json {
                print_json(&body);
            } else {
                render_detail(&body);
            }
        }
        RingGroupsCmd::Set {
            group_id: id,
            settings,
            org,
            confirm,
        } => {
            let id = group_id(&id)?;
            let body = settings_body(&settings)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let change = Change {
                method: "PATCH",
                path: format!("/organizations/{o}/calling/ring-groups/{id}"),
                body,
                verb: "change",
                id: &id,
                org: &o,
            };
            apply(&client, change, confirm).await?;
        }
        RingGroupsCmd::Members {
            group_id: id,
            members,
            body,
            clear,
            org,
            confirm,
        } => {
            let id = group_id(&id)?;
            let body = members_body(member_list(&members, body.as_deref(), clear)?)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let change = Change {
                method: "PUT",
                path: format!("/organizations/{o}/calling/ring-groups/{id}/members"),
                body,
                verb: "replace the members of",
                id: &id,
                org: &o,
            };
            apply(&client, change, confirm).await?;
        }
        RingGroupsCmd::Create { name, settings, org, confirm } => {
            let body = create_body(&name, &settings)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            let path = format!("/organizations/{o}/calling/ring-groups");
            if confirm.dry_run {
                println!("\n  {} would POST {path}", "●".dimmed());
                print_json(&body);
            }
            let target = Target {
                verb: "create",
                noun: "ring group",
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
                println!("\n  {} created — no members yet, and no number routes to it", "✓".green());
                println!("    {}\n", group_line(&created));
            }
        }
        RingGroupsCmd::Delete { group_id: id, org, confirm } => {
            let id = group_id(&id)?;
            let client = require_authed().await?;
            let o = require_active_org(&client, org)?;
            if crate::destructive::confirm_delete("ring group", &id, &o, confirm)? {
                let path = format!("/organizations/{o}/calling/ring-groups/{id}");
                client
                    .delete(&path)
                    .await
                    .map_err(|e| admin_only(e, true))
                    .with_context(|| format!("DELETE {path} (refused while a phone number routes to it)"))?;
                println!("\n  {} deleted ring group {id}\n", "✓".green());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "11111111-2222-3333-4444-555555555555";
    const U1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const U2: &str = "aaaaaaaa-0000-0000-0000-000000000002";

    #[test]
    fn group_ids_must_be_uuids() {
        assert_eq!(group_id(G).unwrap(), G);
        for bad in ["../numbers", "x/members", "", "sales", "11111111-2222-3333-4444-55555555555g"] {
            assert!(group_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn settings_body_sends_only_what_was_given() {
        let body = settings_body(&Settings {
            strategy: Some("round_robin".into()),
            ring_first_seconds: Some(10),
            notify_email: Some(" ar@acme.com ".into()),
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(body, json!({ "strategy": "round_robin", "ringFirstSeconds": 10, "notifyEmail": "ar@acme.com" }));
        assert_eq!(
            settings_body(&Settings {
                notify_email: Some(String::new()),
                message_only: Some(true),
                enabled: Some(false),
                ..Settings::default()
            })
            .unwrap(),
            json!({ "notifyEmail": null, "messageOnly": true, "enabled": false })
        );
        assert!(settings_body(&Settings::default()).is_err());
    }

    #[test]
    fn settings_validation_mirrors_api_prime() {
        for n in [4, 121, 0] {
            assert!(
                settings_body(&Settings {
                    ring_first_seconds: Some(n),
                    ..Settings::default()
                })
                .is_err(),
                "{n}"
            );
        }
        for n in [5, 120] {
            assert!(
                settings_body(&Settings {
                    ring_first_seconds: Some(n),
                    ..Settings::default()
                })
                .is_ok(),
                "{n}"
            );
        }
        assert!(settings_body(&Settings {
            strategy: Some("random".into()),
            ..Settings::default()
        })
        .is_err());
        for bad in ["ar", "ar@acme", "@acme.com", "a@b.com,c@d.com", "a@b.com;c@d.com", "a b@c.com", "ar@acme..com"] {
            assert!(
                settings_body(&Settings {
                    notify_email: Some(bad.into()),
                    ..Settings::default()
                })
                .is_err(),
                "{bad}"
            );
        }
        let long = format!("{}@acme.com", "a".repeat(MAX_NOTIFY_EMAIL));
        assert!(settings_body(&Settings {
            notify_email: Some(long),
            ..Settings::default()
        })
        .is_err());
    }

    #[test]
    fn member_specs_parse_flags_in_ring_order() {
        let list = vec![parse_member(&format!("{U2}:first")).unwrap(), parse_member(&format!("{U1}:off")).unwrap()];
        assert_eq!(
            members_body(list).unwrap(),
            json!({ "members": [{ "userId": U2, "ringFirst": true }, { "userId": U1, "enabled": false }] })
        );
        assert!(parse_member("dana").is_err());
        assert!(parse_member(&format!("{U1}:boss")).is_err());
        assert_eq!(members_body(vec![]).unwrap(), json!({ "members": [] }));
    }

    #[test]
    fn member_lists_are_validated() {
        assert!(members_body(vec![json!({ "userId": U1 }), json!({ "userId": U1 })]).is_err());
        assert!(members_body(vec![json!({ "userId": U1, "position": 2 })]).is_err());
        assert!(members_body(vec![json!({ "userId": U1, "ringFirst": "yes" })]).is_err());
        assert!(members_body(vec![json!("x")]).is_err());
        let many = (0..=MAX_MEMBERS)
            .map(|i| json!({ "userId": format!("aaaaaaaa-0000-0000-0000-{i:012}") }))
            .collect();
        assert!(members_body(many).is_err());
    }

    #[test]
    fn create_body_needs_a_name_and_validates_settings() {
        assert_eq!(create_body(" Billing ", &Settings::default()).unwrap(), json!({ "name": "Billing" }));
        assert_eq!(
            create_body(
                "Sales",
                &Settings {
                    strategy: Some("sequential".into()),
                    notify_email: Some(String::new()),
                    ..Settings::default()
                }
            )
            .unwrap(),
            json!({ "name": "Sales", "strategy": "sequential" })
        );
        assert!(create_body("  ", &Settings::default()).is_err());
        assert!(create_body(&"x".repeat(MAX_NAME + 1), &Settings::default()).is_err());
        assert!(create_body(
            "Sales",
            &Settings {
                ring_first_seconds: Some(200),
                ..Settings::default()
            }
        )
        .is_err());
    }

    #[test]
    fn a_403_reads_as_admin_only() {
        let e = admin_only(anyhow::anyhow!("PATCH /x returned HTTP 403 Forbidden: {{}}"), true);
        assert!(format!("{e:#}").contains("admin only"));
        let e = admin_only(anyhow::anyhow!("GET /x returned HTTP 403 Forbidden: {{}}"), false);
        assert!(format!("{e:#}").contains("calling.use"));
        let e = admin_only(anyhow::anyhow!("PUT /x returned HTTP 500 Internal Server Error: boom"), true);
        assert!(!format!("{e:#}").contains("admin only"));
    }

    #[test]
    fn lines_show_the_department_settings() {
        let l = group_line(&json!({ "id": G, "name": "Sales", "strategy": "sequential", "ringFirstSeconds": 12, "enabled": true, "memberCount": 3 }));
        assert!(l.contains("Sales") && l.contains("ring-first 12s") && l.contains("3 member"), "{l}");
        let l = group_line(&json!({ "id": G, "name": "Billing", "messageOnly": true, "notifyEmail": "ar@acme.com", "enabled": false }));
        assert!(l.contains("message-only → ar@acme.com") && l.contains("[disabled]"), "{l}");
    }
}
