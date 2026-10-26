//! `th api keys …` — Smoo AI auth clients ("API keys").
//!
//! Two client types, both minted here:
//!
//! - **M2M** (`machine-to-machine`): a server/CI credential — returns a
//!   `client_id` + **secret key** (shown once, `smoo_sk_<id>_<secret>` for
//!   new keys). Used for `client_credentials` grants against `auth.smoo.ai`.
//! - **B2M** (`browser-to-machine`): a browser/frontend credential —
//!   returns a **publishable key** (shown once) restricted to an
//!   allowlist of origins. Used by embeddable widgets where the key is
//!   exposed to the page, so origin-pinning is the security boundary.
//!
//! Lifecycle (ADR-162 phase 1, SMOODEV-4208): `active ⇄ disabled → revoked`.
//! `disable`/`enable` pause and resume a key; `revoke` is terminal (the row
//! stays as a tombstone, `list --all` shows it). `rotate` swaps the secret in
//! place — the client id is unchanged and the old secret keeps working for an
//! overlap window. `disable-all`/`enable-all` are the org kill switch:
//! `enable-all` only re-enables keys the switch turned off.
//!
//! The backend (`/organizations/{org_id}/auth-clients`) requires a
//! dashboard **user** session holding `api_keys.manage` and 403s under
//! M2M — a key can't manage keys — so this surface uses [`UserClient`],
//! the user-JWT client. A master admin can target a child org with
//! `--org-id` (user JWT acts cross-org).

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};
use tabled::{
    builder::Builder,
    settings::{
        object::Rows,
        style::{HorizontalLine, Style},
        Color, Modify,
    },
};

use super::print_json;
use crate::smooai::user_client::UserClient;

/// Auth-client kind. The CLI uses the short `m2m`/`b2m` spellings; the
/// API uses the long `machine-to-machine`/`browser-to-machine` forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ClientType {
    /// machine-to-machine — server/CI secret (client_id + secret key).
    #[value(name = "m2m", alias = "machine-to-machine")]
    M2m,
    /// browser-to-machine — origin-restricted publishable key for browsers.
    #[value(name = "b2m", alias = "browser-to-machine")]
    B2m,
}

impl ClientType {
    fn api_value(self) -> &'static str {
        match self {
            Self::M2m => "machine-to-machine",
            Self::B2m => "browser-to-machine",
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// List the org's auth clients — both M2M and B2M — as a table of
    /// name, client id, type, status, last used and expiry. Revoked keys
    /// are hidden unless `--all`.
    List {
        /// Include revoked keys (tombstones) in the list.
        #[arg(long, visible_alias = "include-revoked")]
        all: bool,
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then
        /// the credentials file's `active_org_id`.
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Create an auth client. M2M (default) returns a secret key for
    /// server/CI use; B2M returns a publishable key for the browser and
    /// requires at least one `--origin`. The key is shown EXACTLY ONCE —
    /// store it immediately.
    Create {
        /// A label for the key, shown in `list` and the dashboard
        /// (e.g. `ci-deploy`).
        #[arg(long)]
        name: Option<String>,
        /// Client type: `m2m` (machine-to-machine secret, default) or
        /// `b2m` (browser-to-machine publishable key).
        #[arg(long = "type", value_enum, default_value_t = ClientType::M2m)]
        client_type: ClientType,
        /// Days until the key expires (1–365). The API defaults to 90.
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=365))]
        expires_in_days: Option<u16>,
        /// Allowed origin for a B2M client (repeatable). Required for
        /// `--type b2m`, ignored for m2m — e.g. `--origin
        /// https://app.example.com --origin https://example.com`.
        #[arg(long = "allowed-origin", visible_alias = "origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Raw JSON body escape hatch — a file path, `-` for stdin, or
        /// inline JSON. Overrides every other create flag when given.
        #[arg(long)]
        body: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Update a client's name and/or a B2M client's allowed origins
    /// (`--allowed-origin` replaces the whole list; only B2M clients have
    /// origins, so the API rejects origins on an M2M client).
    Update {
        /// The client id from `th api keys list`.
        client_id: String,
        /// New allowed origin (repeatable). Replaces the existing list.
        #[arg(long = "allowed-origin", visible_alias = "origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
        /// New name for the key.
        #[arg(long)]
        name: Option<String>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty summary.
        #[arg(long)]
        json: bool,
    },
    /// Rename a key (the label shown in `list` and the dashboard).
    Rename {
        /// The client id from `th api keys list`.
        client_id: String,
        /// The new name.
        name: String,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
    /// Disable a key: it stops authenticating immediately (within 30 s at
    /// worst) but can be turned back on with `enable`. Use this to pause a
    /// key while you investigate; `revoke` is the permanent version.
    Disable {
        /// The client id from `th api keys list`.
        client_id: String,
        /// Why — recorded on the key and in the audit log.
        #[arg(long)]
        reason: Option<String>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
    /// Re-enable a disabled key. Revoked and expired keys stay off.
    Enable {
        /// The client id from `th api keys list`.
        client_id: String,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
    /// Revoke an auth client. The key stops working and the action is
    /// permanent — the row stays as a tombstone (`list --all`) but can
    /// never authenticate again. Use `disable` to pause instead.
    #[command(visible_alias = "delete")]
    Revoke {
        /// The client id from `th api keys list`.
        client_id: String,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
        /// Print the target and exit without revoking.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
    /// Rotate a key's secret in place. The client id stays the same; the
    /// new secret is shown once, and the old secret keeps working until
    /// the overlap ends (default 24 h, max 168 h) so consumers can switch
    /// over. Rotating again during an overlap drops the oldest secret.
    Rotate {
        /// The client id to rotate, from `th api keys list`.
        client_id: String,
        /// Hours the old secret keeps working (1–168). The API defaults to 24.
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=168))]
        overlap_hours: Option<u16>,
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the new secret once).
        #[arg(long)]
        json: bool,
        /// Rotating starts the clock on the live secret, so every consumer
        /// still holding it breaks when the overlap ends — confirm first
        /// (SMOODEV-3606).
        #[command(flatten)]
        confirm: crate::destructive::Confirm,
    },
    /// Kill switch: disable EVERY active user-created key in the org at
    /// once (the product's own internal keys are untouched). Interactive
    /// runs make you type the org's slug; scripts need `--yes`. Undo with
    /// `enable-all`.
    DisableAll {
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
        /// Print the target org and exit without disabling anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip typing the org slug. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
    /// Undo `disable-all`: re-enable the keys the kill switch turned off.
    /// Keys someone disabled by hand stay off.
    EnableAll {
        /// Override the active org (see `list` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    run(cmd).await.map_err(explain_forbidden)
}

#[allow(clippy::too_many_lines)] // one arm per verb, same shape as the sibling modules
async fn run(cmd: Cmd) -> Result<()> {
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::List { all, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client.get(&list_path(&org, all)).await.context("GET auth-clients")?;
            if json {
                print_json(&resp);
            } else {
                print_clients(&resp);
            }
        }
        Cmd::Create {
            name,
            client_type,
            expires_in_days,
            allowed_origins,
            org_id,
            body,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let payload = match body {
                Some(b) => super::read_body(&b)?,
                None => build_create_body(client_type, &allowed_origins, name.as_deref(), expires_in_days)?,
            };
            let resp = client
                .post(&format!("/organizations/{org}/auth-clients"), &payload)
                .await
                .context("POST auth-client")?;
            if json {
                print_json(&resp);
            } else {
                print_created(&resp, "auth client created");
            }
        }
        Cmd::Update {
            client_id,
            allowed_origins,
            name,
            org_id,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let payload = build_update_body(name.as_deref(), &allowed_origins)?;
            let resp = client
                .patch(&format!("/organizations/{org}/auth-clients/{client_id}"), &payload)
                .await
                .context("PATCH auth-client")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} updated {}", "✓".green(), client_id.bold());
                if let Some(n) = &name {
                    println!("    {} {}", "name".dimmed(), n);
                }
                print_origins(&resp);
                println!();
            }
        }
        Cmd::Rename { client_id, name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let payload = build_update_body(Some(&name), &[])?;
            let resp = client
                .patch(&format!("/organizations/{org}/auth-clients/{client_id}"), &payload)
                .await
                .context("PATCH auth-client (rename)")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} renamed {} to {}", "✓".green(), client_id.bold(), name.bold());
                println!();
            }
        }
        Cmd::Disable {
            client_id,
            reason,
            org_id,
            json,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(
                    &format!("/organizations/{org}/auth-clients/{client_id}/disable"),
                    &build_disable_body(reason.as_deref()),
                )
                .await
                .context("POST auth-client disable")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} disabled {} — it stops authenticating now", "✓".green(), client_id.bold());
                println!("  {} undo with `th api keys enable {client_id}`", "→".dimmed());
                println!();
            }
        }
        Cmd::Enable { client_id, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/auth-clients/{client_id}/enable"), &json!({}))
                .await
                .context("POST auth-client enable")?;
            if json {
                print_json(&resp);
            } else {
                println!();
                println!("  {} enabled {}", "✓".green(), client_id.bold());
                println!();
            }
        }
        Cmd::Revoke {
            client_id,
            org_id,
            json,
            dry_run,
            yes,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "revoke",
                    noun: "auth client",
                    id: &client_id,
                    org: &org,
                    severity: crate::destructive::Severity::Irreversible,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                let resp = client
                    .delete(&format!("/organizations/{org}/auth-clients/{client_id}"))
                    .await
                    .context("DELETE auth-client")?;
                if json {
                    print_json(&resp);
                } else {
                    println!();
                    println!("  {} revoked client {}", "✓".green(), client_id.bold());
                    println!();
                }
            }
        }
        Cmd::Rotate {
            client_id,
            overlap_hours,
            org_id,
            json,
            confirm,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let proceed = crate::destructive::gate_with(
                &crate::destructive::Target {
                    verb: "rotate",
                    noun: "auth client (its current secret stops working after the overlap)",
                    id: &client_id,
                    org: &org,
                    severity: crate::destructive::Severity::Standard,
                },
                confirm,
            )?;
            if proceed {
                let resp = client
                    .post(
                        &format!("/organizations/{org}/auth-clients/{client_id}/rotate"),
                        &build_rotate_body(overlap_hours),
                    )
                    .await
                    .context("POST auth-client rotate")?;
                if json {
                    print_json(&resp);
                } else {
                    for line in rotated_lines(&resp, &client_id) {
                        println!("{line}");
                    }
                }
            }
        }
        Cmd::DisableAll { org_id, json, dry_run, yes } => {
            let org = crate::active_org::resolve(org_id)?;
            if kill_switch_gate(&client, &org, dry_run, yes).await? {
                let resp = client
                    .post(&format!("/organizations/{org}/auth-clients/disable-all"), &json!({}))
                    .await
                    .context("POST auth-clients disable-all")?;
                if json {
                    print_json(&resp);
                } else {
                    let n = resp.get("disabled").and_then(Value::as_u64).unwrap_or(0);
                    println!();
                    println!("  {} disabled {n} key{} in org {org}", "✓".green().bold(), plural(n));
                    println!("  {} undo with `th api keys enable-all`", "→".dimmed());
                    println!();
                }
            }
        }
        Cmd::EnableAll { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/auth-clients/enable-all"), &json!({}))
                .await
                .context("POST auth-clients enable-all")?;
            if json {
                print_json(&resp);
            } else {
                let n = resp.get("enabled").and_then(Value::as_u64).unwrap_or(0);
                println!();
                println!("  {} re-enabled {n} key{} the kill switch had disabled", "✓".green(), plural(n));
                println!();
            }
        }
    }
    Ok(())
}

/// The API answers `403 {error:'Forbidden', missingPermission:'api_keys.manage'}`
/// when the caller can't manage keys. [`UserClient`] surfaces that as a raw
/// HTTP error; name the permission and who can grant it instead.
fn explain_forbidden(err: anyhow::Error) -> anyhow::Error {
    let text = format!("{err:#}");
    if text.contains("403") && text.contains("api_keys.manage") {
        err.context("you don't have the `api_keys.manage` permission in this org — ask an org admin to grant it (see `th smoo roles`)")
    } else {
        err
    }
}

fn list_path(org: &str, include_revoked: bool) -> String {
    if include_revoked {
        format!("/organizations/{org}/auth-clients?includeRevoked=true")
    } else {
        format!("/organizations/{org}/auth-clients")
    }
}

/// Build the create payload from the structured flags, enforcing the B2M
/// origin requirement client-side with a clear message (the API would
/// otherwise 400). Unset optional fields are omitted so the API defaults
/// (90-day expiry, no name) apply.
fn build_create_body(client_type: ClientType, origins: &[String], name: Option<&str>, expires_in_days: Option<u16>) -> Result<Value> {
    let mut body = Map::new();
    body.insert("type".into(), json!(client_type.api_value()));
    if client_type == ClientType::B2m {
        if origins.is_empty() {
            bail!("--type b2m requires at least one --origin (browser clients are origin-restricted)");
        }
        body.insert("allowedOrigins".into(), json!(origins));
    }
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        body.insert("name".into(), json!(n));
    }
    if let Some(d) = expires_in_days {
        body.insert("expiresInDays".into(), json!(d));
    }
    Ok(Value::Object(body))
}

/// PATCH payload: only the fields being changed. Origins replace the whole
/// list, so an empty `origins` means "leave them alone", not "clear them".
fn build_update_body(name: Option<&str>, origins: &[String]) -> Result<Value> {
    let mut body = Map::new();
    if let Some(n) = name {
        let n = n.trim();
        if n.is_empty() {
            bail!("the key name can't be empty");
        }
        body.insert("name".into(), json!(n));
    }
    if !origins.is_empty() {
        body.insert("allowedOrigins".into(), json!(origins));
    }
    if body.is_empty() {
        bail!("nothing to update — pass --name and/or --allowed-origin (origins replace the client's full list)");
    }
    Ok(Value::Object(body))
}

fn build_disable_body(reason: Option<&str>) -> Value {
    reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map_or_else(|| json!({}), |r| json!({ "reason": r }))
}

fn build_rotate_body(overlap_hours: Option<u16>) -> Value {
    overlap_hours.map_or_else(|| json!({}), |h| json!({ "overlapHours": h }))
}

const fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Confirm the org-wide kill switch. Same rules as [`crate::destructive`]
/// (banner always, `--dry-run` stops, non-interactive needs `--yes`), but the
/// interactive confirmation is the org's slug rather than a key id — the
/// mistake worth catching here is being pointed at the wrong org.
async fn kill_switch_gate(client: &UserClient, org: &str, dry_run: bool, yes: bool) -> Result<bool> {
    use crate::destructive::{decide, Decision};
    use std::io::IsTerminal;

    // Best effort: the org list gives us a human name + slug to type. If it
    // fails (e.g. a master admin acting on a child org), fall back to the id.
    let (name, slug) = client.get("/organizations").await.map_or((None, None), |body| org_name_and_slug(&body, org));
    let token = slug.clone().unwrap_or_else(|| org.to_string());

    println!();
    println!("  {} {}", "about to disable".red().bold(), "EVERY active API key".red().bold());
    println!(
        "    {}  {}{}",
        "org ".dimmed(),
        org.yellow(),
        name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
    );
    println!(
        "    {}  {}",
        "note".dimmed(),
        "integrations using these keys stop working until `enable-all`".yellow()
    );
    println!();

    match decide(dry_run, yes, std::io::stdin().is_terminal()) {
        Decision::DryRun => {
            println!("  {} dry-run — nothing was disabled", "●".dimmed());
            println!();
            Ok(false)
        }
        Decision::Proceed => Ok(true),
        Decision::Prompt => {
            let typed: String = dialoguer::Input::new()
                .with_prompt(format!("type `{token}` to disable every key (anything else aborts)"))
                .allow_empty(true)
                .interact_text()
                .context("read kill-switch confirmation")?;
            if kill_switch_confirmed(&typed, org, name.as_deref(), slug.as_deref()) {
                Ok(true)
            } else {
                anstream::eprintln!("  {} aborted — nothing was disabled", "✗".yellow());
                bail!("aborted by operator")
            }
        }
        Decision::Refuse => bail!(
            "refusing to disable every API key in org {org} without confirmation: not a terminal, so there is no way to ask. \
             Re-run with `--dry-run` to see the target, or `--yes` to confirm you mean this org."
        ),
    }
}

/// Find `org`'s name and slug in a `{data:[…]}` or bare-array orgs payload.
fn org_name_and_slug(body: &Value, org: &str) -> (Option<String>, Option<String>) {
    let items = body.get("data").and_then(Value::as_array).or_else(|| body.as_array());
    let Some(found) = items.and_then(|a| a.iter().find(|o| o.get("id").and_then(Value::as_str) == Some(org))) else {
        return (None, None);
    };
    let field = |k: &str| found.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    (field("name"), field("slug"))
}

/// The typed confirmation must be the org's slug, name or id — exactly
/// (surrounding whitespace aside). Empty never confirms.
fn kill_switch_confirmed(typed: &str, org_id: &str, name: Option<&str>, slug: Option<&str>) -> bool {
    let typed = typed.trim();
    !typed.is_empty() && (typed == org_id || Some(typed) == slug || Some(typed) == name)
}

/// Print a freshly-minted client. The key value (secret for M2M, public
/// for B2M) is returned once, so make it prominent.
fn print_created(resp: &Value, heading: &str) {
    let cid = resp.get("clientId").and_then(Value::as_str).unwrap_or("?");
    let ty = resp.get("type").and_then(Value::as_str).unwrap_or("");
    println!();
    println!("  {} {heading} — key shown once, store it now:", "✓".green().bold());
    println!();
    println!("    {} {}", "client id".dimmed(), cid.bold());
    if let Some(name) = resp.get("name").and_then(Value::as_str) {
        println!("    {} {}", "name".dimmed(), name);
    }
    if !ty.is_empty() {
        println!("    {} {}", "type".dimmed(), ty);
    }
    if let Some(sk) = resp.get("secretKey").and_then(Value::as_str) {
        println!("    {} {}", "secret key".dimmed(), sk.bold());
    }
    if let Some(pk) = resp.get("publicKey").and_then(Value::as_str) {
        println!("    {} {}", "public key".dimmed(), pk.bold());
    }
    if let Some(exp) = resp.get("expiresAt").and_then(Value::as_str) {
        println!("    {} {}", "expires".dimmed(), exp);
    }
    println!();
}

/// The rotate result: the new secret (once) and when the old one dies.
fn rotated_lines(resp: &Value, client_id: &str) -> Vec<String> {
    let cid = resp.get("clientId").and_then(Value::as_str).unwrap_or(client_id);
    let mut out = vec![
        String::new(),
        format!("  {} rotated {} — new secret shown once, store it now:", "✓".green().bold(), cid.bold()),
        String::new(),
    ];
    if let Some(sk) = resp.get("secretKey").and_then(Value::as_str) {
        out.push(format!("    {} {}", "secret key".dimmed(), sk.bold()));
    }
    match resp.get("previousSecretExpiresAt").and_then(Value::as_str) {
        Some(at) => out.push(format!("    {} {}", "old secret stops working".dimmed(), at.yellow())),
        None => out.push(format!("    {} {}", "old secret".dimmed(), "stopped working immediately".yellow())),
    }
    out.push(format!(
        "  {} the client id is unchanged — swap the secret in every consumer before then",
        "→".dimmed()
    ));
    out.push(String::new());
    out
}

/// Print the `allowedOrigins` of an update response, one per line.
fn print_origins(resp: &Value) {
    if let Some(origins) = resp.get("allowedOrigins").and_then(Value::as_array) {
        for o in origins {
            if let Some(s) = o.as_str() {
                println!("    {} {}", "•".dimmed(), s);
            }
        }
    }
}

const LIST_HEADERS: [&str; 6] = ["NAME", "CLIENT ID", "TYPE", "STATUS", "LAST USED", "EXPIRES"];

/// One table row per client: name, client id, type, status, last used,
/// expiry. Pure (no colour) so the shape is unit-testable.
fn client_row(c: &Value) -> [String; 6] {
    let s = |k: &str| c.get(k).and_then(Value::as_str);
    let ty = match s("type") {
        Some("machine-to-machine") => "m2m".to_string(),
        Some("browser-to-machine") => "b2m".to_string(),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    };
    let status = match (s("status"), s("statusReason")) {
        (Some(st), Some(reason)) if st != "active" && !reason.is_empty() => format!("{st} ({reason})"),
        (Some(st), _) => st.to_string(),
        // Pre-lifecycle API responses carry no status: every listed key was live.
        (None, _) => "active".to_string(),
    };
    [
        s("name").filter(|n| !n.is_empty()).unwrap_or("—").to_string(),
        s("clientId").unwrap_or("?").to_string(),
        ty,
        status,
        s("lastUsedAt").map_or_else(|| "never".to_string(), short_ts),
        s("expiresAt").map_or_else(|| "—".to_string(), short_ts),
    ]
}

/// `2026-10-26T14:03:11.123Z` → `2026-10-26 14:03`. Anything that isn't an
/// ISO timestamp is shown as-is.
fn short_ts(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 16 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' {
        format!("{} {}", &s[..10], &s[11..16])
    } else {
        s.to_string()
    }
}

/// Render the list as a table string (B2M origins listed beneath it).
fn render_clients(items: &[Value]) -> String {
    use std::fmt::Write as _;

    let mut b = Builder::default();
    b.push_record(LIST_HEADERS);
    for c in items {
        b.push_record(client_row(c));
    }
    let mut table = b.build();
    table
        .with(
            Style::modern_rounded()
                .horizontals([(1, HorizontalLine::inherit(Style::modern_rounded()))])
                .remove_horizontal(),
        )
        .with(Modify::new(Rows::first()).with(Color::BOLD | Color::FG_CYAN));
    let mut out = table.to_string();
    for c in items {
        let origins: Vec<&str> = c
            .get("allowedOrigins")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !origins.is_empty() {
            let id = c.get("clientId").and_then(Value::as_str).unwrap_or("?");
            let _ = write!(out, "\n  {id} origins: {}", origins.join(", "));
        }
    }
    out
}

/// Pretty-print the bare-array list of auth clients.
fn print_clients(resp: &Value) {
    println!();
    let Some(items) = resp.as_array() else {
        print_json(resp);
        return;
    };
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), "no auth clients".dimmed());
        println!();
        return;
    }
    println!("{}", render_clients(items));
    println!();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is the idiom for test assertions")]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> Cmd {
        let mut full = vec!["keys"];
        full.extend_from_slice(args);
        Harness::try_parse_from(full).expect("parses").cmd
    }

    fn parse_err(args: &[&str]) -> bool {
        let mut full = vec!["keys"];
        full.extend_from_slice(args);
        Harness::try_parse_from(full).is_err()
    }

    #[test]
    fn m2m_body_has_no_origins() {
        let body = build_create_body(ClientType::M2m, &[], None, None).expect("m2m needs no origins");
        assert_eq!(
            body,
            json!({ "type": "machine-to-machine" }),
            "unset optionals are omitted so API defaults apply"
        );
    }

    #[test]
    fn b2m_requires_origins() {
        assert!(build_create_body(ClientType::B2m, &[], None, None).is_err(), "b2m with no origins must error");
        let body = build_create_body(ClientType::B2m, &["https://a.example.com".into(), "https://b.example.com".into()], None, None).expect("b2m with origins");
        assert_eq!(body["type"], "browser-to-machine");
        assert_eq!(body["allowedOrigins"], json!(["https://a.example.com", "https://b.example.com"]));
    }

    #[test]
    fn create_body_carries_name_and_expiry() {
        let body = build_create_body(ClientType::M2m, &["https://ignored.example.com".into()], Some("  ci-deploy "), Some(30)).expect("m2m");
        assert_eq!(body, json!({ "type": "machine-to-machine", "name": "ci-deploy", "expiresInDays": 30 }));
        let blank = build_create_body(ClientType::M2m, &[], Some("   "), None).expect("m2m");
        assert!(blank.get("name").is_none(), "a blank name is omitted, not sent as empty");
    }

    #[test]
    fn client_type_api_values() {
        assert_eq!(ClientType::M2m.api_value(), "machine-to-machine");
        assert_eq!(ClientType::B2m.api_value(), "browser-to-machine");
    }

    #[test]
    fn update_body_only_sends_changed_fields() {
        assert_eq!(build_update_body(Some("prod"), &[]).expect("name"), json!({ "name": "prod" }));
        assert_eq!(
            build_update_body(None, &["https://x.example.com".into()]).expect("origins"),
            json!({ "allowedOrigins": ["https://x.example.com"] })
        );
        assert!(build_update_body(None, &[]).is_err(), "an empty update is refused");
        assert!(build_update_body(Some("  "), &[]).is_err(), "a blank rename is refused");
    }

    #[test]
    fn disable_and_rotate_bodies() {
        assert_eq!(build_disable_body(Some(" leaked in a log ")), json!({ "reason": "leaked in a log" }));
        assert_eq!(build_disable_body(Some("")), json!({}));
        assert_eq!(build_disable_body(None), json!({}));
        assert_eq!(build_rotate_body(Some(48)), json!({ "overlapHours": 48 }));
        assert_eq!(build_rotate_body(None), json!({}), "omitted so the API's 24 h default applies");
    }

    #[test]
    fn list_path_includes_revoked_only_with_all() {
        assert_eq!(list_path("o1", false), "/organizations/o1/auth-clients");
        assert_eq!(list_path("o1", true), "/organizations/o1/auth-clients?includeRevoked=true");
    }

    #[test]
    fn parses_list_all_and_alias() {
        assert!(matches!(parse(&["list", "--all", "--json"]), Cmd::List { all: true, json: true, .. }));
        assert!(matches!(parse(&["list", "--include-revoked"]), Cmd::List { all: true, .. }));
        assert!(matches!(parse(&["list"]), Cmd::List { all: false, .. }));
    }

    #[test]
    fn parses_create_new_flags() {
        match parse(&[
            "create",
            "--name",
            "ci",
            "--type",
            "b2m",
            "--expires-in-days",
            "30",
            "--origin",
            "https://a.example.com",
        ]) {
            Cmd::Create {
                name,
                client_type,
                expires_in_days,
                allowed_origins,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("ci"));
                assert_eq!(client_type, ClientType::B2m);
                assert_eq!(expires_in_days, Some(30));
                assert_eq!(allowed_origins, vec!["https://a.example.com"]);
            }
            _ => panic!("expected Create"),
        }
        // The old spelling still works.
        assert!(matches!(
            parse(&["create", "--type", "b2m", "--allowed-origin", "https://a.example.com"]),
            Cmd::Create { .. }
        ));
        // Expiry is bounded 1..=365 at parse time.
        assert!(parse_err(&["create", "--expires-in-days", "0"]));
        assert!(parse_err(&["create", "--expires-in-days", "366"]));
        assert!(matches!(
            parse(&["create", "--expires-in-days", "365"]),
            Cmd::Create {
                expires_in_days: Some(365),
                ..
            }
        ));
    }

    #[test]
    fn parses_lifecycle_verbs() {
        match parse(&["rename", "cid-1", "Prod deploy"]) {
            Cmd::Rename { client_id, name, .. } => {
                assert_eq!(client_id, "cid-1");
                assert_eq!(name, "Prod deploy");
            }
            _ => panic!("expected Rename"),
        }
        match parse(&["disable", "cid-1", "--reason", "leaked"]) {
            Cmd::Disable { client_id, reason, .. } => {
                assert_eq!(client_id, "cid-1");
                assert_eq!(reason.as_deref(), Some("leaked"));
            }
            _ => panic!("expected Disable"),
        }
        assert!(matches!(parse(&["enable", "cid-1", "--org", "o1"]), Cmd::Enable { .. }));
        assert!(matches!(parse(&["enable-all"]), Cmd::EnableAll { .. }));
        assert!(matches!(parse(&["disable-all", "--yes"]), Cmd::DisableAll { yes: true, dry_run: false, .. }));
        assert!(matches!(parse(&["disable-all", "--dry-run"]), Cmd::DisableAll { dry_run: true, .. }));
    }

    #[test]
    fn parses_rotate_overlap_bounds() {
        match parse(&["rotate", "cid-1", "--overlap-hours", "168", "--yes"]) {
            Cmd::Rotate {
                client_id,
                overlap_hours,
                confirm,
                ..
            } => {
                assert_eq!(client_id, "cid-1");
                assert_eq!(overlap_hours, Some(168));
                assert!(confirm.yes);
            }
            _ => panic!("expected Rotate"),
        }
        assert!(parse_err(&["rotate", "cid-1", "--overlap-hours", "169"]));
        assert!(parse_err(&["rotate", "cid-1", "--overlap-hours", "0"]));
        assert!(matches!(parse(&["rotate", "cid-1"]), Cmd::Rotate { overlap_hours: None, .. }));
    }

    #[test]
    fn revoke_keeps_delete_alias() {
        assert!(matches!(parse(&["revoke", "cid-1", "--yes"]), Cmd::Revoke { yes: true, .. }));
        match parse(&["delete", "cid-1", "--dry-run"]) {
            Cmd::Revoke { client_id, dry_run, .. } => {
                assert_eq!(client_id, "cid-1");
                assert!(dry_run);
            }
            _ => panic!("`delete` should alias `revoke`"),
        }
    }

    #[test]
    fn row_renders_lifecycle_fields() {
        let row = client_row(&json!({
            "clientId": "abc123",
            "type": "machine-to-machine",
            "name": "ci",
            "status": "disabled",
            "statusReason": "kill_switch",
            "lastUsedAt": "2026-10-25T09:15:42.000Z",
            "expiresAt": "2027-01-23T00:00:00Z"
        }));
        assert_eq!(
            row,
            ["ci", "abc123", "m2m", "disabled (kill_switch)", "2026-10-25 09:15", "2027-01-23 00:00"].map(String::from)
        );
    }

    #[test]
    fn row_defaults_for_legacy_and_sparse_clients() {
        let row = client_row(&json!({ "clientId": "old", "type": "browser-to-machine", "name": null }));
        assert_eq!(row, ["—", "old", "b2m", "active", "never", "—"].map(String::from));
        // An active key's reason is noise; only show it for non-active states.
        let active = client_row(&json!({ "clientId": "x", "type": "machine-to-machine", "status": "active", "statusReason": "manual" }));
        assert_eq!(active[3], "active");
    }

    #[test]
    fn short_ts_formats_iso_only() {
        assert_eq!(short_ts("2026-10-26T14:03:11.123Z"), "2026-10-26 14:03");
        assert_eq!(short_ts("yesterday"), "yesterday");
        assert_eq!(short_ts("2026-10-26"), "2026-10-26");
    }

    #[test]
    fn table_has_headers_rows_and_origins() {
        let items = vec![
            json!({ "clientId": "m2m-1", "type": "machine-to-machine", "name": "ci", "status": "active" }),
            json!({ "clientId": "b2m-1", "type": "browser-to-machine", "status": "revoked", "allowedOrigins": ["https://a.example.com"] }),
        ];
        let out = render_clients(&items);
        for needle in [
            "NAME",
            "CLIENT ID",
            "LAST USED",
            "EXPIRES",
            "m2m-1",
            "b2m-1",
            "revoked",
            "b2m-1 origins: https://a.example.com",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
        }
        assert!(!out.contains("m2m-1 origins"), "m2m clients have no origins line");
    }

    #[test]
    fn rotate_output_shows_secret_and_cutoff() {
        let resp = json!({ "clientId": "cid-1", "secretKey": "smoo_sk_0123_abc", "previousSecretExpiresAt": "2026-10-27T12:00:00Z" });
        let out = rotated_lines(&resp, "cid-1").join("\n");
        assert!(out.contains("smoo_sk_0123_abc"));
        assert!(out.contains("old secret stops working"));
        assert!(out.contains("2026-10-27T12:00:00Z"));
        let immediate = rotated_lines(&json!({ "secretKey": "s" }), "cid-2").join("\n");
        assert!(immediate.contains("cid-2"), "falls back to the requested id");
        assert!(immediate.contains("stopped working immediately"));
    }

    #[test]
    fn kill_switch_confirmation_matches_org_identifiers_only() {
        let (org, name, slug) = ("org-uuid", Some("Acme Inc"), Some("acme"));
        assert!(kill_switch_confirmed("acme", org, name, slug));
        assert!(kill_switch_confirmed("  acme\n", org, name, slug));
        assert!(kill_switch_confirmed("Acme Inc", org, name, slug));
        assert!(kill_switch_confirmed("org-uuid", org, name, slug));
        assert!(!kill_switch_confirmed("", org, name, slug));
        assert!(!kill_switch_confirmed("y", org, name, slug));
        assert!(!kill_switch_confirmed("ACME", org, name, slug), "exact match only");
        assert!(!kill_switch_confirmed("", "", None, None), "empty never confirms, even against an empty id");
    }

    #[test]
    fn org_lookup_reads_envelope_or_array() {
        let body = json!({ "data": [{ "id": "o1", "name": "Acme", "slug": "acme" }, { "id": "o2", "name": "Other", "slug": "" }] });
        assert_eq!(org_name_and_slug(&body, "o1"), (Some("Acme".into()), Some("acme".into())));
        assert_eq!(org_name_and_slug(&body, "o2"), (Some("Other".into()), None), "blank slug falls back to the id");
        assert_eq!(org_name_and_slug(&json!([{ "id": "o3", "slug": "three" }]), "o3"), (None, Some("three".into())));
        assert_eq!(org_name_and_slug(&body, "missing"), (None, None));
    }

    #[test]
    fn forbidden_error_names_the_permission() {
        let raw = anyhow::anyhow!(r#"POST https://api.smoo.ai/x returned HTTP 403 Forbidden: {{"error":"Forbidden","missingPermission":"api_keys.manage"}}"#);
        let msg = format!("{:#}", explain_forbidden(raw));
        assert!(msg.starts_with("you don't have the `api_keys.manage` permission"), "{msg}");
        assert!(msg.contains("returned HTTP 403"), "the raw error is kept: {msg}");
        let other = format!("{:#}", explain_forbidden(anyhow::anyhow!("returned HTTP 500: boom")));
        assert_eq!(other, "returned HTTP 500: boom");
    }
}
