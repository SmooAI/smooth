//! `th smoo admin org overrides list | set | remove` — per-org product access
//! overrides (SMOODEV-3714, the CLI twin of the SMOODEV-3711 API).
//!
//! An override grants (or denies) one feature to one org regardless of what it
//! has bought — pilots, MSA terms, comps. Every call goes to
//! `/admin/organizations/{org}/overrides…`, gated server-side by
//! `requireSuperAdmin`; the server also validates the feature key, the reason,
//! the expiry and the telephony-only allowance fields. This module mirrors the
//! cheap checks client-side so a typo fails before any request:
//!
//! - the org is a UUID, the reason is non-blank and ≤ 500 chars, `--expires-at`
//!   is RFC 3339 with an offset and in the future;
//! - `--included-voice-minutes` / `--overage-cents` only with `telephony`, and
//!   they are sent only when given (the server rejects them on any other key,
//!   even as `null`).
//!
//! Writes read the current override first (which also 404s an unknown org
//! before any prompt), print org + feature + before/after + host, confirm on a
//! TTY and refuse off a TTY without `--yes` (`crate::destructive::gate`).

use anstream::println;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Args, Subcommand};
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use super::client::{print_ok, AdminClient};
use super::render::{render, Format, TableOptions};
use crate::destructive::{gate_with, Confirm, Severity, Target};

/// Columns for the overrides table, in reading order.
const OVERRIDE_COLUMNS: &[&str] = &[
    "featureKey",
    "enabled",
    "expired",
    "expiresAt",
    "reason",
    "includedVoiceMinutes",
    "voiceMinutesOverageRateCents",
    "updatedAt",
];

/// The server's `reason` cap (`z.string().trim().min(1).max(500)`).
const REASON_MAX: usize = 500;

/// The one feature key that carries a PSTN allowance.
const TELEPHONY: &str = "telephony";

#[derive(Debug, Subcommand)]
pub enum OverridesCommands {
    /// List an org's overrides, expired ones included.
    #[command(visible_alias = "ls")]
    List {
        /// Org UUID.
        org_id: String,
        /// Print the raw JSON response instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Grant or deny one feature to an org (replaces any existing override).
    ///
    /// The PUT replaces the whole override: omitting `--expires-at` makes it
    /// permanent, and omitting the telephony allowance flags clears them.
    Set(SetArgs),
    /// Remove an override so the org falls back to what its products grant.
    #[command(visible_alias = "rm")]
    Remove {
        /// Org UUID.
        org_id: String,
        /// Feature key (e.g. `crm`, `telephony`).
        feature_key: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Print the server response as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("state").required(true).args(["enabled", "disabled"])))]
pub struct SetArgs {
    /// Org UUID.
    org_id: String,
    /// Feature key (e.g. `crm`, `telephony`). The server rejects unknown keys.
    feature_key: String,
    /// Grant the feature.
    #[arg(long)]
    enabled: bool,
    /// Deny the feature, even when a product would grant it.
    #[arg(long)]
    disabled: bool,
    /// Why — kept on the override for the audit trail (≤ 500 chars).
    #[arg(long)]
    reason: String,
    /// Expiry as RFC 3339 with an offset (`2026-12-31T23:59:59Z`), in the
    /// future. Omit for a permanent override.
    #[arg(long, value_name = "RFC3339")]
    expires_at: Option<String>,
    /// telephony only: PSTN minutes included per month.
    #[arg(long, value_name = "N")]
    included_voice_minutes: Option<u32>,
    /// telephony only: overage rate in cents per minute.
    #[arg(long = "overage-cents", value_name = "CENTS")]
    overage_cents: Option<u32>,
    #[command(flatten)]
    confirm: Confirm,
    /// Print the server response as JSON.
    #[arg(long)]
    json: bool,
}

/// Parse and canonicalise (lowercase, hyphenated) the org UUID. The server
/// 400s a non-UUID too, but only after the session load and a round trip.
fn parse_org(raw: &str) -> Result<String> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| anyhow::anyhow!("org `{raw}` is not a UUID — see `smoo admin org list --search <name>`"))
}

/// Normalise a feature key. The server owns the list of valid keys; here we
/// only reject what can't be a key at all (and would mangle the path).
fn parse_feature_key(raw: &str) -> Result<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("feature key `{raw}` is not valid — use the key as the API names it (e.g. `crm`, `telephony`)");
    }
    Ok(key)
}

/// `--expires-at` → an RFC 3339 instant strictly after `now`, in UTC.
fn parse_expires_at(raw: &str, now: DateTime<Utc>) -> Result<String> {
    let at = DateTime::parse_from_rfc3339(raw.trim())
        .with_context(|| format!("`--expires-at {raw}` is not RFC 3339 with an offset (e.g. `2026-12-31T23:59:59Z`)"))?
        .with_timezone(&Utc);
    if at <= now {
        bail!("`--expires-at {raw}` is in the past — an override expiry must be in the future (omit it for a permanent override)");
    }
    Ok(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Build the PUT body, validating it the way the server will.
///
/// # Errors
/// Blank or over-long reason, a bad/past expiry, or an allowance flag on a
/// feature other than `telephony`.
fn build_set_body(args: &SetArgs, feature_key: &str, now: DateTime<Utc>) -> Result<Value> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        bail!("`--reason` cannot be blank — it is the audit trail for this override");
    }
    if reason.chars().count() > REASON_MAX {
        bail!("`--reason` is {} chars; the server caps it at {REASON_MAX}", reason.chars().count());
    }
    let mut body = Map::new();
    body.insert("enabled".into(), json!(args.enabled));
    body.insert("reason".into(), json!(reason));
    if let Some(raw) = &args.expires_at {
        body.insert("expiresAt".into(), json!(parse_expires_at(raw, now)?));
    }
    let allowance = [
        ("includedVoiceMinutes", "--included-voice-minutes", args.included_voice_minutes),
        ("voiceMinutesOverageRateCents", "--overage-cents", args.overage_cents),
    ];
    for (field, flag, value) in allowance {
        let Some(v) = value else { continue };
        if feature_key != TELEPHONY {
            bail!("`{flag}` only applies to the `{TELEPHONY}` override, not `{feature_key}`");
        }
        body.insert(field.into(), json!(v));
    }
    Ok(Value::Object(body))
}

/// The `overrides` array of a list response (a bare array is tolerated).
fn override_rows(body: &Value) -> &[Value] {
    body.get("overrides")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .map_or(&[], Vec::as_slice)
}

/// The override for `feature_key` in a list response, if any.
fn find_override<'a>(body: &'a Value, feature_key: &str) -> Option<&'a Value> {
    override_rows(body)
        .iter()
        .find(|o| o.get("featureKey").and_then(Value::as_str) == Some(feature_key))
}

/// `Acme Corp (1111…)` from the list response's `organization`, else the id.
fn org_label(body: &Value, org: &str) -> String {
    body.get("organization")
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| org.to_string(), |name| format!("{name} ({org})"))
}

/// One-line summary of an override (a stored row or a PUT body):
/// `enabled · until 2026-12-31T00:00:00Z · 500 min incl · 3¢/min over`.
fn describe(o: &Value) -> String {
    let mut parts = vec![match o.get("enabled").and_then(Value::as_bool) {
        Some(true) => "enabled".to_string(),
        Some(false) => "disabled".to_string(),
        None => "?".to_string(),
    }];
    match o.get("expiresAt").and_then(Value::as_str) {
        Some(at) => parts.push(format!("until {at}")),
        None => parts.push("permanent".to_string()),
    }
    if o.get("expired").and_then(Value::as_bool) == Some(true) {
        parts.push("EXPIRED".to_string());
    }
    if let Some(m) = o.get("includedVoiceMinutes").and_then(Value::as_u64) {
        parts.push(format!("{m} min incl"));
    }
    if let Some(c) = o.get("voiceMinutesOverageRateCents").and_then(Value::as_u64) {
        parts.push(format!("{c}¢/min over"));
    }
    parts.join(" · ")
}

fn override_table() -> TableOptions {
    TableOptions::default().with_label("overrides").with_columns(OVERRIDE_COLUMNS)
}

async fn list_overrides(client: &AdminClient, org: &str) -> Result<Value> {
    client
        .get(&format!("/admin/organizations/{org}/overrides"))
        .await
        .with_context(|| format!("list overrides of org {org}"))
}

fn override_path(org: &str, feature_key: &str) -> String {
    format!("/admin/organizations/{org}/overrides/{}", urlencoding::encode(feature_key))
}

pub async fn dispatch(cmd: OverridesCommands) -> Result<()> {
    // Validate every flag before loading (and possibly refreshing) a session.
    match cmd {
        OverridesCommands::List { org_id, json } => {
            let org = parse_org(&org_id)?;
            let client = AdminClient::from_user_session().await?;
            let body = list_overrides(&client, &org).await?;
            print_list(&body, &org, json);
        }
        OverridesCommands::Set(args) => {
            let org = parse_org(&args.org_id)?;
            let feature_key = parse_feature_key(&args.feature_key)?;
            let body = build_set_body(&args, &feature_key, Utc::now())?;
            let client = AdminClient::from_user_session().await?;
            set(&client, &org, &feature_key, &body, args.confirm, args.json).await?;
        }
        OverridesCommands::Remove {
            org_id,
            feature_key,
            confirm,
            json,
        } => {
            let org = parse_org(&org_id)?;
            let feature_key = parse_feature_key(&feature_key)?;
            let client = AdminClient::from_user_session().await?;
            remove(&client, &org, &feature_key, confirm, json).await?;
        }
    }
    Ok(())
}

fn print_list(body: &Value, org: &str, json: bool) {
    if json {
        render(body, Format::Json, &TableOptions::default());
        return;
    }
    println!("{} {}", "Org:".bold().cyan(), org_label(body, org));
    let rows = override_rows(body);
    if rows.is_empty() {
        println!("No overrides on this org — it gets exactly what its products grant. This is a confirmed read, not a read failure.");
        return;
    }
    render(&json!(rows), Format::Table, &override_table());
}

async fn set(client: &AdminClient, org: &str, feature_key: &str, body: &Value, confirm: Confirm, json: bool) -> Result<()> {
    // Read first: 404s an unknown org before the prompt, and shows what the
    // PUT is about to replace.
    let current = list_overrides(client, org).await?;
    println!();
    println!("  {}  {}", "org    ".dimmed(), org_label(&current, org).yellow());
    println!(
        "  {}  {}",
        "before ".dimmed(),
        find_override(&current, feature_key).map_or_else(|| "(no override)".to_string(), describe)
    );
    println!("  {}  {}", "after  ".dimmed(), describe(body).bold());
    if let Some(reason) = body.get("reason").and_then(Value::as_str) {
        println!("  {}  {reason}", "reason ".dimmed());
    }
    let target = Target {
        // "grant" → "granted"; a denial is spelled "block" so it inflects.
        verb: if body.get("enabled").and_then(Value::as_bool) == Some(true) {
            "grant"
        } else {
            "block"
        },
        noun: "feature",
        id: feature_key,
        org,
        severity: Severity::Standard,
    };
    if !gate_with(&target, confirm)? {
        return Ok(());
    }

    let resp = client
        .put(&override_path(org, feature_key), body)
        .await
        .with_context(|| format!("set override `{feature_key}` on org {org}"))?;
    let created = resp.get("created").and_then(Value::as_bool).unwrap_or(false);
    let stored = resp.get("override").unwrap_or(&resp);
    print_ok(format!(
        "{} override `{feature_key}` on {org}: {}",
        if created { "created" } else { "updated" },
        describe(stored)
    ));
    if json {
        render(&resp, Format::Json, &TableOptions::default());
    } else {
        render(&json!([stored]), Format::Table, &override_table());
    }
    Ok(())
}

async fn remove(client: &AdminClient, org: &str, feature_key: &str, confirm: Confirm, json: bool) -> Result<()> {
    let current = list_overrides(client, org).await?;
    let Some(existing) = find_override(&current, feature_key) else {
        print_ok(format!("no `{feature_key}` override on {} — nothing to remove", org_label(&current, org)));
        if json {
            render(&json!({ "removed": false }), Format::Json, &TableOptions::default());
        }
        return Ok(());
    };
    println!();
    println!("  {}  {}", "org    ".dimmed(), org_label(&current, org).yellow());
    println!("  {}  {}", "current".dimmed(), describe(existing));
    let target = Target {
        verb: "remove",
        noun: "override",
        id: feature_key,
        org,
        severity: Severity::Standard,
    };
    if !gate_with(&target, confirm)? {
        return Ok(());
    }

    let resp = client
        .delete(&override_path(org, feature_key))
        .await
        .with_context(|| format!("remove override `{feature_key}` from org {org}"))?;
    if resp.get("removed").and_then(Value::as_bool) == Some(false) {
        // Someone else removed it between our read and the DELETE.
        print_ok(format!("`{feature_key}` override on {org} was already gone — nothing removed"));
    } else {
        print_ok(format!("removed override `{feature_key}` from {org}"));
    }
    if json {
        render(&resp, Format::Json, &TableOptions::default());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use clap::Parser;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";

    #[derive(Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: OverridesCommands,
    }

    fn parse(args: &[&str]) -> Result<OverridesCommands, clap::Error> {
        let mut full = vec!["overrides"];
        full.extend_from_slice(args);
        Wrap::try_parse_from(full).map(|w| w.cmd)
    }

    fn set_args(args: &[&str]) -> SetArgs {
        let mut full = vec!["set", ORG];
        full.extend_from_slice(args);
        match parse(&full).unwrap() {
            OverridesCommands::Set(a) => a,
            other => panic!("expected set, got {other:?}"),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    // ── arg parsing ────────────────────────────────────────────────────

    #[test]
    fn list_and_remove_parse() {
        assert!(matches!(parse(&["list", ORG, "--json"]).unwrap(), OverridesCommands::List { json: true, .. }));
        assert!(matches!(parse(&["ls", ORG]).unwrap(), OverridesCommands::List { json: false, .. }));
        let OverridesCommands::Remove { feature_key, confirm, .. } = parse(&["rm", ORG, "crm", "--yes"]).unwrap() else {
            panic!("expected remove");
        };
        assert_eq!(feature_key, "crm");
        assert!(confirm.yes && !confirm.dry_run);
    }

    #[test]
    fn set_needs_exactly_one_of_enabled_or_disabled() {
        let neither = parse(&["set", ORG, "crm", "--reason", "pilot"]).expect_err("neither must be rejected");
        assert_eq!(neither.kind(), clap::error::ErrorKind::MissingRequiredArgument, "{neither}");
        let both = parse(&["set", ORG, "crm", "--enabled", "--disabled", "--reason", "pilot"]).expect_err("both must be rejected");
        assert_eq!(both.kind(), clap::error::ErrorKind::ArgumentConflict, "{both}");
    }

    #[test]
    fn set_requires_a_reason() {
        let err = parse(&["set", ORG, "crm", "--enabled"]).expect_err("missing --reason must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument, "{err}");
    }

    #[test]
    fn set_parses_every_flag() {
        let a = set_args(&[
            "telephony",
            "--enabled",
            "--reason",
            "MSA pilot",
            "--expires-at",
            "2027-01-01T00:00:00Z",
            "--included-voice-minutes",
            "500",
            "--overage-cents",
            "3",
            "--dry-run",
        ]);
        assert!(a.enabled && !a.disabled);
        assert_eq!(a.included_voice_minutes, Some(500));
        assert_eq!(a.overage_cents, Some(3));
        assert!(a.confirm.dry_run);
        assert!(parse(&["set", ORG, "telephony", "--enabled", "--reason", "x", "--overage-cents", "-1"]).is_err());
    }

    // ── validation + body building ─────────────────────────────────────

    #[test]
    fn org_and_feature_key_are_validated() {
        assert_eq!(parse_org(&ORG.to_uppercase()).unwrap(), ORG);
        assert!(parse_org("acme").is_err());
        assert_eq!(parse_feature_key(" CRM ").unwrap(), "crm");
        assert_eq!(parse_feature_key("social_command_center").unwrap(), "social_command_center");
        for bad in ["", "  ", "crm/../x", "a b", "crm?x=1"] {
            assert!(parse_feature_key(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn minimal_body_is_enabled_and_reason_only() {
        let a = set_args(&["crm", "--enabled", "--reason", "  comp for launch partner  "]);
        assert_eq!(
            build_set_body(&a, "crm", now()).unwrap(),
            json!({ "enabled": true, "reason": "comp for launch partner" })
        );
        let a = set_args(&["crm", "--disabled", "--reason", "churned"]);
        assert_eq!(build_set_body(&a, "crm", now()).unwrap(), json!({ "enabled": false, "reason": "churned" }));
    }

    #[test]
    fn expiry_is_normalised_to_utc_and_must_be_future() {
        let a = set_args(&["crm", "--enabled", "--reason", "pilot", "--expires-at", "2026-12-31T19:00:00-05:00"]);
        assert_eq!(build_set_body(&a, "crm", now()).unwrap()["expiresAt"], json!("2027-01-01T00:00:00Z"));
        for bad in ["2026-10-07T12:00:00Z", "2020-01-01T00:00:00Z"] {
            let a = set_args(&["crm", "--enabled", "--reason", "pilot", "--expires-at", bad]);
            let err = build_set_body(&a, "crm", now()).unwrap_err().to_string();
            assert!(err.contains("in the past"), "{bad}: {err}");
        }
        for bad in ["2026-12-31", "tomorrow", "2026-12-31T00:00:00"] {
            let a = set_args(&["crm", "--enabled", "--reason", "pilot", "--expires-at", bad]);
            let err = build_set_body(&a, "crm", now()).unwrap_err().to_string();
            assert!(err.contains("RFC 3339"), "{bad}: {err}");
        }
    }

    #[test]
    fn reason_must_be_non_blank_and_capped() {
        let a = set_args(&["crm", "--enabled", "--reason", "   "]);
        assert!(build_set_body(&a, "crm", now()).unwrap_err().to_string().contains("blank"));
        let long = "x".repeat(REASON_MAX + 1);
        let a = set_args(&["crm", "--enabled", "--reason", &long]);
        assert!(build_set_body(&a, "crm", now()).unwrap_err().to_string().contains("500"));
        let exact = "é".repeat(REASON_MAX);
        let a = set_args(&["crm", "--enabled", "--reason", &exact]);
        assert!(build_set_body(&a, "crm", now()).is_ok(), "500 multibyte chars is within the cap");
    }

    #[test]
    fn telephony_allowance_is_sent_only_when_given_and_only_for_telephony() {
        let a = set_args(&[
            "telephony",
            "--enabled",
            "--reason",
            "pilot",
            "--included-voice-minutes",
            "500",
            "--overage-cents",
            "3",
        ]);
        assert_eq!(
            build_set_body(&a, "telephony", now()).unwrap(),
            json!({ "enabled": true, "reason": "pilot", "includedVoiceMinutes": 500, "voiceMinutesOverageRateCents": 3 })
        );
        let a = set_args(&["telephony", "--enabled", "--reason", "pilot"]);
        let body = build_set_body(&a, "telephony", now()).unwrap();
        assert!(body.get("includedVoiceMinutes").is_none() && body.get("voiceMinutesOverageRateCents").is_none());
        let a = set_args(&["crm", "--enabled", "--reason", "pilot", "--overage-cents", "3"]);
        let err = build_set_body(&a, "crm", now()).unwrap_err().to_string();
        assert!(err.contains("--overage-cents") && err.contains("telephony"), "{err}");
    }

    // ── response parsing ───────────────────────────────────────────────

    fn list_body() -> Value {
        json!({
            "organization": { "id": ORG, "name": "Acme" },
            "overrides": [
                { "featureKey": "crm", "enabled": true, "expiresAt": null, "reason": "comp", "expired": false },
                { "featureKey": "telephony", "enabled": true, "expiresAt": "2026-01-01T00:00:00Z", "reason": "pilot",
                  "includedVoiceMinutes": 500, "voiceMinutesOverageRateCents": 3, "expired": true },
            ]
        })
    }

    #[test]
    fn finds_overrides_by_key() {
        let body = list_body();
        assert_eq!(override_rows(&body).len(), 2);
        assert_eq!(find_override(&body, "telephony").unwrap()["reason"], json!("pilot"));
        assert!(find_override(&body, "workflows").is_none());
        assert!(find_override(&json!({ "overrides": [] }), "crm").is_none());
        assert_eq!(override_rows(&json!([{ "featureKey": "crm" }])).len(), 1);
    }

    #[test]
    fn labels_and_descriptions_read_well() {
        assert_eq!(org_label(&list_body(), ORG), format!("Acme ({ORG})"));
        assert_eq!(org_label(&json!({}), ORG), ORG);
        let body = list_body();
        assert_eq!(describe(find_override(&body, "crm").unwrap()), "enabled · permanent");
        assert_eq!(
            describe(find_override(&body, "telephony").unwrap()),
            "enabled · until 2026-01-01T00:00:00Z · EXPIRED · 500 min incl · 3¢/min over"
        );
        assert_eq!(describe(&json!({ "enabled": false, "reason": "x" })), "disabled · permanent");
    }

    // ── against a fake api.smoo.ai (the real reqwest + AdminClient path) ──

    async fn fake_api(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    /// A handler that records it was called and answers `body`.
    fn recording(hit: &std::sync::Arc<std::sync::atomic::AtomicBool>, body: Value) -> axum::routing::MethodRouter {
        let hit = hit.clone();
        axum::routing::any(move || {
            let hit = hit.clone();
            let body = body.clone();
            async move {
                hit.store(true, std::sync::atomic::Ordering::SeqCst);
                axum::Json(body)
            }
        })
    }

    #[tokio::test]
    async fn set_puts_the_built_body_to_the_feature_path() {
        use axum::{extract::Path, routing::put, Json};
        let router = axum::Router::new().route(
            "/admin/organizations/{org}/overrides/{key}",
            put(|Path((org, key)): Path<(String, String)>, Json(body): Json<Value>| async move {
                assert_eq!(org, ORG);
                assert_eq!(key, "telephony");
                assert_eq!(body, json!({ "enabled": true, "reason": "pilot", "includedVoiceMinutes": 500 }));
                Json(json!({ "override": { "featureKey": key, "enabled": true, "expiresAt": null, "expired": false, "includedVoiceMinutes": 500 }, "created": true }))
            }),
        );
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let a = set_args(&["telephony", "--enabled", "--reason", "pilot", "--included-voice-minutes", "500", "--yes"]);
        let body = build_set_body(&a, "telephony", now()).unwrap();
        let resp = client.put(&override_path(ORG, "telephony"), &body).await.unwrap();
        assert_eq!(resp["created"], json!(true));
        assert_eq!(describe(&resp["override"]), "enabled · permanent · 500 min incl");
    }

    #[tokio::test]
    async fn remove_with_no_override_never_deletes() {
        use axum::{routing::get, Json};
        use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
        let deleted = Arc::new(AtomicBool::new(false));
        let router = axum::Router::new()
            .route(
                "/admin/organizations/{org}/overrides",
                get(|| async { Json(json!({ "organization": { "id": ORG, "name": "Acme" }, "overrides": [] })) }),
            )
            .route("/admin/organizations/{org}/overrides/{key}", recording(&deleted, json!({ "removed": true })));
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        remove(&client, ORG, "crm", Confirm { dry_run: false, yes: true }, true).await.unwrap();
        assert!(!deleted.load(Ordering::SeqCst), "DELETE must not be sent when there is no override");
    }

    #[tokio::test]
    async fn remove_deletes_an_existing_override() {
        use axum::{routing::get, Json};
        use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
        let deleted = Arc::new(AtomicBool::new(false));
        let router = axum::Router::new()
            .route("/admin/organizations/{org}/overrides", get(|| async { Json(list_body()) }))
            .route("/admin/organizations/{org}/overrides/{key}", recording(&deleted, json!({ "removed": true })));
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        remove(&client, ORG, "crm", Confirm { dry_run: false, yes: true }, false).await.unwrap();
        assert!(deleted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dry_run_set_reads_but_never_writes() {
        use axum::{routing::get, Json};
        use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
        let written = Arc::new(AtomicBool::new(false));
        let router = axum::Router::new()
            .route("/admin/organizations/{org}/overrides", get(|| async { Json(list_body()) }))
            .route("/admin/organizations/{org}/overrides/{key}", recording(&written, json!({ "created": false })));
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let body = json!({ "enabled": false, "reason": "churned" });
        set(&client, ORG, "crm", &body, Confirm { dry_run: true, yes: false }, false).await.unwrap();
        assert!(!written.load(Ordering::SeqCst), "PUT must not be sent on --dry-run");
    }

    #[tokio::test]
    async fn unknown_org_404_surfaces_the_server_message() {
        use axum::{http::StatusCode, routing::get, Json};
        let router = axum::Router::new().route(
            "/admin/organizations/{org}/overrides",
            get(|| async { (StatusCode::NOT_FOUND, Json(json!({ "message": "Organization not found" }))) }),
        );
        let client = AdminClient::with_base(fake_api(router).await, "tok");
        let msg = format!("{:#}", list_overrides(&client, ORG).await.unwrap_err());
        assert!(msg.contains("404") && msg.contains("Organization not found"), "{msg}");
    }
}
