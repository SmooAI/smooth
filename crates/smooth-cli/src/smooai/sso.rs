//! `th smoo sso connections|link|require` — enterprise SSO, the CLI twin of the
//! hosted MCP's `sso_*` tools and the copilot's `sso.*` tools (SMOODEV-4052).
//!
//! Every verb calls the same api-prime route the SSO settings page calls
//! (`/organizations/{org}/sso-connections…`) with your own sign-in, so the
//! route decides: `org.sso.manage` plus the `sso` product on every call. A 403
//! is reported as "admin only".
//!
//! - `connections list|show` read the connections (never the IdP metadata XML).
//! - `link preview <connection>` lists every existing account in the verified
//!   domains (linked, `not_linked`, conflict) and the Require SSO lockout count;
//!   `link apply <connection> <email>…` links an explicit list (at most 200).
//! - `require on|off <connection>`: ON refuses every non-SSO sign-in for the
//!   verified domains, so an account that is not linked is LOCKED OUT. `on`
//!   prints the preview and refuses unless `--acknowledge-lockout <N>` equals the
//!   fresh lockout count (0 included), then confirms like every write and reads
//!   the count once more after the confirm (it may have moved while you read).
//!   It is refused outright while the connection cannot link (not registered,
//!   or no verified domain: the plan lists nobody, yet the sign-in block starts
//!   the moment it becomes ready) and when the plan is truncated (more than 1000
//!   accounts, so the count is only a lower bound).
//!
//! The rules mirror smooai's shared validator
//! (`rust/smooth-operator-agent/src/tools_org_admin.rs`): connection ids are
//! UUIDs, emails are trimmed + lowercased + de-duplicated, at most 200 per apply.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use smooth_api_client::SmoothApiClient;

use super::{print_json, require_active_org, require_authed};
use crate::destructive::{Confirm, Severity, Target};

/// `MAX_EMAILS_PER_APPLY` in smooai api-prime `handlers/sso_account_links.rs`.
pub const MAX_LINK_EMAILS: usize = 200;

#[derive(Subcommand)]
pub enum Cmd {
    /// The org's SSO connections.
    #[command(visible_alias = "conn")]
    Connections {
        #[command(subcommand)]
        cmd: ConnectionsCmd,
    },
    /// Link existing accounts to a connection's identity provider.
    Link {
        #[command(subcommand)]
        cmd: LinkCmd,
    },
    /// Turn Require SSO on or off for a connection.
    Require {
        #[command(subcommand)]
        cmd: RequireCmd,
    },
}

#[derive(Subcommand)]
pub enum ConnectionsCmd {
    /// List the connections: status, domains, Require SSO, JIT, auto-link.
    #[command(visible_alias = "ls")]
    List {
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// One connection.
    Show {
        /// The connection id (from `th smoo sso connections list`).
        connection_id: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum LinkCmd {
    /// Every existing account in the verified domains with its link status,
    /// and how many accounts Require SSO would lock out. Changes nothing.
    Preview {
        /// The connection id.
        connection_id: String,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Link the listed accounts (at most 200). All or nothing: a missing,
    /// SSO-born or conflicting account refuses the batch.
    Apply {
        /// The connection id.
        connection_id: String,
        /// The accounts to link, by email.
        #[arg(required = true)]
        emails: Vec<String>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RequireCmd {
    /// SSO-only sign-in for the verified domains. Locks out every account that
    /// is not linked; needs `--acknowledge-lockout <N>` with the current count.
    On {
        /// The connection id.
        connection_id: String,
        /// The lockout count from `th smoo sso link preview` (0 included).
        #[arg(long = "acknowledge-lockout")]
        acknowledge_lockout: Option<i64>,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Allow password and other sign-in again.
    Off {
        /// The connection id.
        connection_id: String,
        #[command(flatten)]
        confirm: Confirm,
        /// Override the active org.
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
}

// ── validation (mirrors tools_org_admin.rs) ──────────────────────────────────

/// A connection id bound for a path segment: a UUID, so it cannot re-point the
/// request at another route.
pub fn connection_id(id: &str) -> Result<String> {
    let id = id.trim();
    let ok = id.len() == 36
        && id
            .chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    if !ok {
        bail!("SSO connection id must be a uuid (from `th smoo sso connections list`); got {id:?}");
    }
    Ok(id.to_ascii_lowercase())
}

/// The `POST account-links` body: trimmed, lowercased, de-duplicated, ≤ 200.
pub fn link_body(emails: &[String]) -> Result<Value> {
    let mut out: Vec<String> = Vec::with_capacity(emails.len());
    for raw in emails {
        let email = raw.trim().to_lowercase();
        if email.is_empty() {
            continue;
        }
        let ok =
            email.len() <= 320 && !email.starts_with('@') && email.rfind('@').is_some_and(|at| at + 1 < email.len()) && !email.chars().any(char::is_whitespace);
        if !ok {
            bail!("{email:?} is not an email address");
        }
        if !out.contains(&email) {
            out.push(email);
        }
    }
    if out.is_empty() {
        bail!("name at least one account to link (see `th smoo sso link preview`)");
    }
    if out.len() > MAX_LINK_EMAILS {
        bail!("link at most {MAX_LINK_EMAILS} accounts at a time");
    }
    Ok(json!({ "emails": out }))
}

/// The plan's counts: `(linked, not_linked, conflict, members_at_risk)`.
pub fn counts(plan: &Value) -> (usize, usize, usize, usize) {
    let (mut linked, mut not_linked, mut conflict, mut members) = (0, 0, 0, 0);
    for a in plan.get("accounts").and_then(Value::as_array).into_iter().flatten() {
        let at_risk = match s(a, "status") {
            "linked" => {
                linked += 1;
                false
            }
            "conflict" => {
                conflict += 1;
                true
            }
            _ => {
                not_linked += 1;
                true
            }
        };
        if at_risk && a.get("orgMember").and_then(Value::as_bool) == Some(true) {
            members += 1;
        }
    }
    (linked, not_linked, conflict, members)
}

/// Accounts Require SSO would lock out: not linked, or conflicting.
pub fn lockout_count(plan: &Value) -> usize {
    let (_, not_linked, conflict, _) = counts(plan);
    not_linked + conflict
}

fn not_ready_text(reason: &str) -> &'static str {
    match reason {
        "not_saml" => "only SAML connections can link accounts",
        "not_registered" => "the identity provider is not registered yet",
        "no_verified_domain" => "no domain is verified yet",
        _ => "it is not ready",
    }
}

/// Turning Require SSO ON needs a ready connection, an exact (untruncated)
/// plan, and the exact current lockout count.
pub fn require_check(plan: &Value, acknowledged: Option<i64>) -> Result<()> {
    if !on(plan, "ready") {
        bail!(
            "not changed: Require SSO cannot be turned on yet: {}. No account can be linked until then, and the sign-in block would start the moment the connection becomes ready, locking out everyone not yet linked",
            not_ready_text(s(plan, "notReadyReason"))
        );
    }
    let locked = lockout_count(plan);
    if on(plan, "truncated") {
        bail!(
            "not changed: these verified domains hold more than 1000 accounts, so the lockout count ({locked} so far) is only a lower bound — link the accounts first, then turn Require SSO on from Settings → SSO"
        );
    }
    if acknowledged == i64::try_from(locked).ok() {
        return Ok(());
    }
    bail!(
        "not changed: Require SSO would lock out {locked} account(s) that are not linked to the identity provider — link them first, or pass --acknowledge-lockout {locked}"
    )
}

// ── rendering ────────────────────────────────────────────────────────────────

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn on(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool) == Some(true)
}

/// One connection as a line — an allowlist, never the metadata XML.
#[must_use]
pub fn connection_line(c: &Value) -> String {
    let domains: Vec<String> = c
        .get("domainVerifications")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|d| format!("{} ({})", s(d, "domain"), s(d, "status"))).collect())
        .unwrap_or_default();
    format!(
        "{} ({}) — {} {}, provider {}, domains [{}], require SSO {}, JIT {}, auto-link {}",
        s(c, "label"),
        s(c, "id"),
        s(c, "protocol"),
        s(c, "status"),
        if c.get("supabaseSsoProviderId").is_some_and(|v| !v.is_null()) {
            "registered"
        } else {
            "NOT registered"
        },
        domains.join(", "),
        if on(c, "ssoRequired") { "ON" } else { "off" },
        if on(c, "jitEnabled") { "on" } else { "off" },
        if on(c, "autoLinkNewAccounts") { "on" } else { "off" },
    )
}

fn render_plan(plan: &Value) {
    println!();
    if !on(plan, "ready") {
        println!(
            "  {} not ready to link ({}) — nothing can be linked, and Require SSO cannot be turned on until it is ready",
            "●".yellow(),
            plan.get("notReadyReason").and_then(Value::as_str).unwrap_or("unknown")
        );
        println!();
        return;
    }
    let (linked, not_linked, conflict, members) = counts(plan);
    let domains: Vec<&str> = plan
        .get("verifiedDomains")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    println!("  verified domains: {}", domains.join(", "));
    for a in plan.get("accounts").and_then(Value::as_array).into_iter().flatten() {
        let mark = match s(a, "status") {
            "linked" => "✓".green().to_string(),
            "conflict" => "✗".red().to_string(),
            _ => "○".yellow().to_string(),
        };
        let member = if on(a, "orgMember") { "member" } else { "not a member" };
        println!("  {mark} {} — {} ({member})", s(a, "email"), s(a, "status"));
    }
    let more = if on(plan, "truncated") { "+" } else { "" };
    println!(
        "\n  {linked} linked, {not_linked}{more} not linked, {conflict} conflicting — Require SSO would lock out {} account(s) ({members} members)",
        format!("{}{more}", not_linked + conflict).bold()
    );
    println!();
}

fn render_link_result(out: &Value) {
    let list = |k: &str| -> Vec<&str> {
        out.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let linked = list("linked");
    let already = list("alreadyLinked");
    println!("\n  {} linked {} account(s) {}", "✓".green(), linked.len(), linked.join(", "));
    if !already.is_empty() {
        println!("  {} already linked: {}", "●".dimmed(), already.join(", "));
    }
    println!();
}

/// A 403 from the SSO routes is the permission answer, not an outage.
fn admin_only(err: anyhow::Error) -> anyhow::Error {
    if err.to_string().contains("HTTP 403") {
        return err.context("admin only: SSO settings need org.sso.manage and the SSO feature on your plan — nothing was changed");
    }
    err
}

fn gate(verb: &str, id: &str, org: &str, confirm: Confirm) -> Result<bool> {
    crate::destructive::gate_with(
        &Target {
            verb,
            noun: "SSO connection",
            id,
            org,
            severity: Severity::Standard,
        },
        confirm,
    )
}

async fn get(client: &SmoothApiClient, path: &str) -> Result<Value> {
    client.get(path).await.map_err(admin_only).with_context(|| format!("GET {path}"))
}

#[allow(clippy::too_many_lines)] // one flat dispatch arm per verb, like the sibling modules
pub async fn cmd(cmd: Cmd) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        Cmd::Connections {
            cmd: ConnectionsCmd::List { org, json },
        } => {
            let o = require_active_org(&client, org)?;
            let body = get(&client, &format!("/organizations/{o}/sso-connections")).await?;
            if json {
                print_json(&body);
                return Ok(());
            }
            let list = body.as_array().cloned().unwrap_or_default();
            println!();
            if list.is_empty() {
                println!("  {} {}", "●".dimmed(), "no SSO connections".dimmed());
            }
            for c in &list {
                println!("  {} {}", "○".dimmed(), connection_line(c));
            }
            println!();
        }
        Cmd::Connections {
            cmd: ConnectionsCmd::Show { connection_id: id, org, json },
        } => {
            let id = connection_id(&id)?;
            let o = require_active_org(&client, org)?;
            let body = get(&client, &format!("/organizations/{o}/sso-connections/{id}")).await?;
            if json {
                print_json(&body);
            } else {
                println!("\n  {}\n", connection_line(&body));
            }
        }
        Cmd::Link {
            cmd: LinkCmd::Preview { connection_id: id, org, json },
        } => {
            let id = connection_id(&id)?;
            let o = require_active_org(&client, org)?;
            let plan = get(&client, &format!("/organizations/{o}/sso-connections/{id}/account-links")).await?;
            if json {
                print_json(&plan);
            } else {
                render_plan(&plan);
            }
        }
        Cmd::Link {
            cmd: LinkCmd::Apply {
                connection_id: id,
                emails,
                confirm,
                org,
            },
        } => {
            let id = connection_id(&id)?;
            let body = link_body(&emails)?;
            let o = require_active_org(&client, org)?;
            let n = body["emails"].as_array().map_or(0, Vec::len);
            if !gate(&format!("link {n} account(s) to"), &id, &o, confirm)? {
                return Ok(());
            }
            let path = format!("/organizations/{o}/sso-connections/{id}/account-links");
            let out = client
                .post(&path, Some(&body))
                .await
                .map_err(admin_only)
                .with_context(|| format!("POST {path}"))?;
            render_link_result(&out);
        }
        Cmd::Require {
            cmd:
                RequireCmd::On {
                    connection_id: id,
                    acknowledge_lockout,
                    confirm,
                    org,
                },
        } => {
            let id = connection_id(&id)?;
            let o = require_active_org(&client, org)?;
            let path = format!("/organizations/{o}/sso-connections/{id}");
            let current = get(&client, &path).await?;
            if on(&current, "ssoRequired") {
                println!("\n  {} Require SSO is already on — nothing changed\n", "●".dimmed());
                return Ok(());
            }
            let plan = get(&client, &format!("{path}/account-links")).await?;
            render_plan(&plan);
            require_check(&plan, acknowledge_lockout)?;
            if !gate("require SSO on", &id, &o, confirm)? {
                return Ok(());
            }
            // The confirm can sit on a TTY for minutes: read the count again.
            let fresh = get(&client, &format!("{path}/account-links")).await?;
            require_check(&fresh, acknowledge_lockout).context("the lockout count changed while you confirmed")?;
            patch_required(&client, &path, true).await?;
        }
        Cmd::Require {
            cmd: RequireCmd::Off {
                connection_id: id,
                confirm,
                org,
            },
        } => {
            let id = connection_id(&id)?;
            let o = require_active_org(&client, org)?;
            let path = format!("/organizations/{o}/sso-connections/{id}");
            if !gate("allow non-SSO sign-in on", &id, &o, confirm)? {
                return Ok(());
            }
            patch_required(&client, &path, false).await?;
        }
    }
    Ok(())
}

async fn patch_required(client: &SmoothApiClient, path: &str, required: bool) -> Result<()> {
    let row = client
        .patch(path, &json!({ "ssoRequired": required }))
        .await
        .map_err(admin_only)
        .with_context(|| format!("PATCH {path}"))?;
    println!("\n  {} {}\n", "✓".green(), connection_line(&row));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const C: &str = "11111111-2222-3333-4444-555555555555";

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

    fn plan() -> Value {
        json!({ "ready": true, "verifiedDomains": ["acme.com"], "truncated": false, "accounts": [
            { "email": "a@acme.com", "status": "linked", "orgMember": true },
            { "email": "b@acme.com", "status": "not_linked", "orgMember": true },
            { "email": "c@acme.com", "status": "conflict", "orgMember": false }
        ] })
    }

    #[test]
    fn every_verb_parses() {
        assert!(matches!(
            parse(&["connections", "ls"]).unwrap(),
            Cmd::Connections {
                cmd: ConnectionsCmd::List { .. }
            }
        ));
        assert!(matches!(
            parse(&["connections", "show", C, "--json"]).unwrap(),
            Cmd::Connections {
                cmd: ConnectionsCmd::Show { json: true, .. }
            }
        ));
        assert!(matches!(parse(&["link", "preview", C]).unwrap(), Cmd::Link { cmd: LinkCmd::Preview { .. } }));
        match parse(&["link", "apply", C, "a@acme.com", "b@acme.com", "--yes"]).unwrap() {
            Cmd::Link {
                cmd: LinkCmd::Apply { emails, confirm, .. },
            } => {
                assert_eq!(emails.len(), 2);
                assert!(confirm.yes);
            }
            _ => panic!("expected link apply"),
        }
        assert!(parse(&["link", "apply", C]).is_err(), "apply needs at least one email");
        match parse(&["require", "on", C, "--acknowledge-lockout", "3", "--dry-run"]).unwrap() {
            Cmd::Require {
                cmd: RequireCmd::On {
                    acknowledge_lockout, confirm, ..
                },
            } => {
                assert_eq!(acknowledge_lockout, Some(3));
                assert!(confirm.dry_run);
            }
            _ => panic!("expected require on"),
        }
        assert!(matches!(parse(&["require", "off", C]).unwrap(), Cmd::Require { cmd: RequireCmd::Off { .. } }));
    }

    #[test]
    fn connection_ids_must_be_uuids() {
        assert_eq!(connection_id(&format!(" {C} ")).unwrap(), C);
        for bad in ["../default-member-role", "x/account-links", "", "okta"] {
            assert!(connection_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn link_body_normalizes_and_bounds() {
        assert_eq!(
            link_body(&[" A@Acme.com".into(), "a@acme.com".into(), "b@acme.com".into()]).unwrap(),
            json!({ "emails": ["a@acme.com", "b@acme.com"] })
        );
        assert!(link_body(&[]).is_err());
        for bad in ["acme.com", "@acme.com", "a@", "a b@acme.com"] {
            assert!(link_body(&[bad.into()]).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_LINK_EMAILS).map(|i| format!("u{i}@acme.com")).collect();
        assert!(link_body(&many).is_err());
        assert!(link_body(&many[..MAX_LINK_EMAILS]).is_ok());
    }

    #[test]
    fn require_on_needs_the_exact_lockout_count() {
        assert_eq!(counts(&plan()), (1, 1, 1, 1));
        assert_eq!(lockout_count(&plan()), 2);
        let err = require_check(&plan(), None).unwrap_err().to_string();
        assert!(err.contains("--acknowledge-lockout 2"), "{err}");
        assert!(require_check(&plan(), Some(1)).is_err());
        assert!(require_check(&plan(), Some(2)).is_ok());
        let clean = json!({ "ready": true, "accounts": [] });
        assert!(require_check(&clean, None).is_err(), "0 must still be acknowledged");
        assert!(require_check(&clean, Some(0)).is_ok());
    }

    #[test]
    fn require_on_is_refused_on_an_unready_connection_whatever_the_ack() {
        for reason in ["not_registered", "no_verified_domain", "not_saml"] {
            let plan = json!({ "ready": false, "notReadyReason": reason, "accounts": [] });
            assert_eq!(lockout_count(&plan), 0, "an unready plan lists nobody");
            let err = require_check(&plan, Some(0)).unwrap_err().to_string();
            assert!(err.contains("cannot be turned on yet"), "{reason}: {err}");
        }
    }

    #[test]
    fn require_on_is_refused_on_a_truncated_plan_whatever_the_ack() {
        let mut plan = plan();
        plan["truncated"] = json!(true);
        let err = require_check(&plan, Some(2)).unwrap_err().to_string();
        assert!(err.contains("lower bound"), "{err}");
    }

    #[test]
    fn connection_line_is_an_allowlist() {
        let line = connection_line(&json!({
            "id": C, "label": "Okta", "protocol": "saml", "status": "active", "supabaseSsoProviderId": "p",
            "samlMetadataXml": "<SECRET/>", "ssoRequired": true,
            "domainVerifications": [{ "domain": "acme.com", "status": "verified" }]
        }));
        assert!(!line.contains("SECRET"), "{line}");
        assert!(line.contains("acme.com (verified)") && line.contains("require SSO ON"), "{line}");
    }

    #[test]
    fn a_403_reads_as_admin_only() {
        let e = admin_only(anyhow::anyhow!("PATCH https://api/x returned HTTP 403 Forbidden: {{}}"));
        assert!(
            format!("{e:#}").starts_with("admin only") && format!("{e:#}").contains("org.sso.manage"),
            "{e:#}"
        );
        assert!(!format!("{:#}", admin_only(anyhow::anyhow!("HTTP 500"))).contains("admin only"));
    }
}
