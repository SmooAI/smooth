//! Smoo AI platform CLI commands. All go through `smooth_api_client`
//! against `api.smoo.ai`. Resources are split into submodules; this
//! file keeps the auth flow (`login` / `logout` / `whoami`) and the
//! org commands because every other command needs an authenticated
//! client + active org id and that machinery lives here.
//!
//! Helper: `require_active_org(&client)` resolves the `--org` flag
//! → `SMOOAI_ORG_ID` env → `active_org_id` in credentials, in that
//! order. Most resource commands take an `Option<String>` for `--org`
//! and call this helper.

pub mod agents;
pub mod crm;
pub mod jobs;
pub mod keys;
pub mod knowledge;
pub mod members;
pub mod observability;
pub mod products;
pub mod profile;
pub mod testing;
pub mod user_client;

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Input, Password};
use owo_colors::OwoColorize;
use smooth_api_client::auth::{client_credentials_grant, token_url};
use smooth_api_client::{CredentialsStore, SmoothApiClient};

/// Build an authenticated client or fail with the standard "run
/// `th api login`" message. Every resource command starts with this.
///
/// Triggers a silent token refresh first if creds exist but are
/// expired AND we have stored client_id/client_secret. So a stale
/// session re-mints transparently — the user doesn't see the
/// expiry unless their stored M2M credentials were rotated.
pub async fn require_authed() -> Result<SmoothApiClient> {
    let client = SmoothApiClient::from_disk().context("load credentials")?;
    if client.credentials().is_none() {
        anyhow::bail!("not logged in — run `th api login` first");
    }
    // Try to refresh if expired. ensure_fresh_token is a no-op when
    // the token is still valid or when no client_credentials are on
    // disk to re-exchange with.
    client.ensure_fresh_token().await.ok();
    if !client.is_authenticated() {
        anyhow::bail!(
            "session expired and no stored client credentials to auto-refresh — run `th api login` again \
             (or set SMOOAI_CONFIG_CLIENT_ID + SMOOAI_CONFIG_CLIENT_SECRET so the next call refreshes silently)"
        );
    }
    Ok(client)
}

/// Resolve the active org id. Delegates to
/// [`crate::active_org::resolve`] so every `th api` subcommand reads
/// from the same source `th config` and `th auth whoami` do.
///
/// The `_client` parameter is retained for API stability (callers
/// across this crate pass it through), but is unused — the shared
/// helper reads directly from the credential stores on disk.
///
/// Order:
///   1. `--org` flag (the `override_org` argument)
///   2. `SMOOAI_ORG_ID` env (handy for CI scripts)
///   3. `active_org_id` from any of: legacy `smooth-api-client`
///      store, client-shared M2M store, client-shared User store
pub fn require_active_org(_client: &SmoothApiClient, override_org: Option<String>) -> Result<String> {
    crate::active_org::resolve(override_org)
}

/// Read a JSON body from `path` (or stdin when `path == "-"`).
pub fn read_body(path: &str) -> Result<serde_json::Value> {
    let raw = if path == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read stdin")?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))?
    };
    serde_json::from_str(&raw).with_context(|| format!("parse JSON from {path}"))
}

/// Pretty-print a JSON value to stdout with a leading + trailing
/// blank line so command output looks consistent with the rest of
/// the CLI.
pub fn print_json(body: &serde_json::Value) {
    println!();
    println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
    println!();
}

/// Pretty-print a `{"data": [...]}` collection envelope as a compact
/// list. Each entry shows whichever of `id`, `name`, `email`,
/// `status` are present. Falls back to full JSON when the shape
/// doesn't match the envelope.
pub fn print_list_envelope(body: &serde_json::Value, item_label: &str) {
    let items = body.get("data").and_then(|v| v.as_array()).or_else(|| body.as_array());
    let Some(items) = items else {
        print_json(body);
        return;
    };
    println!();
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), format!("no {item_label}").dimmed());
        println!();
        return;
    }
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("email").and_then(|v| v.as_str()))
            .unwrap_or("");
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let suffix = if status.is_empty() { String::new() } else { format!(" [{status}]") };
        println!("  {} {} {}{}", "○".dimmed(), id.cyan(), name.bold(), suffix.dimmed());
    }
    println!();
}

/// `th api login` — exchange a client_credentials pair for a bearer
/// JWT against `https://auth.smoo.ai/token` and persist it.
///
/// Credential resolution order (first present wins):
///   1. `--client-id` + `--client-secret` flags
///   2. `SMOOAI_CLIENT_ID` + `SMOOAI_CLIENT_SECRET` env vars (our own)
///   3. `SMOOAI_CONFIG_CLIENT_ID` + `SMOOAI_CONFIG_CLIENT_SECRET`
///      env vars (the `@smooai/config` convention — set by direnv
///      when you're cd'd into the smooai monorepo). Also picks up
///      `SMOOAI_CONFIG_ORG_ID` to seed `active_org_id` so the user
///      skips `th api orgs switch` afterward.
///   4. Interactive dialoguer prompt.
pub async fn cmd_login(client_id: Option<String>, client_secret: Option<String>) -> Result<()> {
    let resolved = resolve_credentials(client_id, client_secret)?;

    println!();
    if let Some(ref source) = resolved.source {
        println!("  {} Using credentials from {}", "●".cyan(), source.dimmed());
    }
    println!("  {} Exchanging client_credentials at {}", "●".cyan(), token_url().dimmed());

    let http = reqwest::Client::builder()
        .user_agent(format!("smooth-cli/{} (https://github.com/SmooAI/smooth)", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build http client")?;
    let mut creds = client_credentials_grant(&http, &resolved.client_id, &resolved.client_secret)
        .await
        .context("client_credentials_grant")?;

    // If `@smooai/config`'s direnv block also exports
    // `SMOOAI_CONFIG_ORG_ID`, seed it as the active org. Saves the
    // user a `th api orgs switch <id>` step after login.
    if creds.active_org_id.is_none() {
        if let Ok(org_id) = std::env::var("SMOOAI_CONFIG_ORG_ID") {
            if !org_id.trim().is_empty() {
                creds.active_org_id = Some(org_id);
            }
        }
    }

    let store = CredentialsStore::default_path()?;
    store.save(&creds).context("save credentials")?;

    println!();
    println!("  {} {}", "✓".green().bold(), "Logged in".green().bold());
    if let Some(ref u) = creds.user {
        println!("    {}  {}", "Identity".dimmed(), u.cyan());
    }
    if let Some(ref o) = creds.active_org_id {
        println!("    {}  {}", "Org     ".dimmed(), o.cyan());
    }
    if let Some(exp) = creds.expires_at {
        let remaining = exp - chrono::Utc::now();
        let label = if remaining.num_hours() >= 1 {
            format!("{}h", remaining.num_hours())
        } else {
            format!("{}m", remaining.num_minutes())
        };
        println!("    {}  {} {}", "Expires ".dimmed(), label.green(), "from now".dimmed());
    }
    println!("    {}  {}", "Saved   ".dimmed(), store.path().display().to_string().dimmed());
    println!();
    if creds.active_org_id.is_some() {
        println!("  {} {}", "→".dimmed(), "next: `th api whoami` to confirm, or `th api orgs list`.".dimmed());
    } else {
        println!(
            "  {} {}",
            "→".dimmed(),
            "next: `th api orgs list` to see your orgs, then `th api orgs switch <id>`.".dimmed()
        );
    }
    println!();
    Ok(())
}

/// Outcome of credential resolution. `source` is `Some("<env var
/// name>")` / `Some("@smooai/config")` / `Some("--client-id flag")` so
/// the login command can tell the user which knob it picked up.
struct ResolvedCredentials {
    client_id: String,
    client_secret: String,
    source: Option<String>,
}

/// Walk the resolution chain. Returns the first complete pair found,
/// or falls back to interactive prompts (no `source` set in that case).
fn resolve_credentials(flag_id: Option<String>, flag_secret: Option<String>) -> Result<ResolvedCredentials> {
    // 1. Flags. Require BOTH so we don't mix a flag-set id with a
    //    secret from somewhere else (which is almost always a typo).
    match (flag_id.filter(|s| !s.trim().is_empty()), flag_secret.filter(|s| !s.trim().is_empty())) {
        (Some(id), Some(secret)) => {
            return Ok(ResolvedCredentials {
                client_id: id,
                client_secret: secret,
                source: Some("--client-id / --client-secret flags".into()),
            })
        }
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("pass BOTH --client-id and --client-secret (or neither — env vars / prompt take over)");
        }
        (None, None) => {}
    }

    // 2. Our own env-var pair.
    if let (Ok(id), Ok(secret)) = (std::env::var("SMOOAI_CLIENT_ID"), std::env::var("SMOOAI_CLIENT_SECRET")) {
        if !id.trim().is_empty() && !secret.trim().is_empty() {
            return Ok(ResolvedCredentials {
                client_id: id,
                client_secret: secret,
                source: Some("$SMOOAI_CLIENT_ID / $SMOOAI_CLIENT_SECRET".into()),
            });
        }
    }

    // 3. The @smooai/config convention. direnv pulls these in for
    //    anyone working inside the smooai monorepo, which is the most
    //    common case for `th api login` right now.
    if let (Ok(id), Ok(secret)) = (std::env::var("SMOOAI_CONFIG_CLIENT_ID"), std::env::var("SMOOAI_CONFIG_CLIENT_SECRET")) {
        if !id.trim().is_empty() && !secret.trim().is_empty() {
            return Ok(ResolvedCredentials {
                client_id: id,
                client_secret: secret,
                source: Some("@smooai/config ($SMOOAI_CONFIG_CLIENT_ID / $SMOOAI_CONFIG_CLIENT_SECRET)".into()),
            });
        }
    }

    // 4. Interactive prompts. No `source` — the user is literally
    //    typing the values into a TTY.
    let id = Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt("Client ID")
        .interact_text()
        .context("read client id from prompt")?;
    let secret = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("Client Secret")
        .interact()
        .context("read client secret from prompt")?;
    Ok(ResolvedCredentials {
        client_id: id,
        client_secret: secret,
        source: None,
    })
}

/// `th logout` — delete the credentials file. Idempotent.
pub async fn cmd_logout() -> Result<()> {
    let store = CredentialsStore::default_path()?;
    let existed = store.load().context("load credentials")?.is_some();
    store.delete().context("delete credentials")?;
    println!();
    if existed {
        println!("  🔴 Logged out");
    } else {
        println!("  {} {}", "●".dimmed(), "Already logged out".dimmed());
    }
    println!();
    Ok(())
}

/// `th api whoami` — show who you are and what you can see.
///
/// Triggers the same auto-refresh path the resource commands do, so a
/// stale token gets re-minted before we report. Then makes two light
/// network calls (`GET /profile` for the human identity behind the
/// client credentials, `GET /organizations/{id}` for the active org
/// name) plus an optional members count if the membership endpoint is
/// allowed for this principal. Each call is tolerant of failure — if
/// the endpoint 403s or 404s for this principal we just skip that
/// row, so M2M clients (which often can't read `/profile`) still see
/// useful output.
pub async fn cmd_whoami() -> Result<()> {
    let client = SmoothApiClient::from_disk().context("load credentials")?;
    let Some(creds) = client.credentials() else {
        println!();
        println!("  {} {}", "●".yellow(), "Not logged in — run `th api login`".yellow());
        println!();
        return Ok(());
    };

    // Refresh before reporting so the user sees the real state, not
    // the stale state. ensure_fresh_token is a no-op when the token
    // is still valid.
    let _ = client.ensure_fresh_token().await;
    // Re-read after refresh — the access_token / expires_at may have
    // rotated underneath us.
    let creds = client.credentials().unwrap_or(creds);

    println!();
    if let Some(ref u) = creds.user {
        println!("  {}  {}", "Identity   ".dimmed(), u.cyan().bold());
    } else {
        println!("  {}  {}", "Identity   ".dimmed(), "(unknown)".dimmed());
    }

    // /profile — only works for user-bearer tokens, often 403/404
    // for M2M client_credentials. Best-effort.
    if let Ok(profile) = client.get("/profile").await {
        let email = profile.get("email").and_then(|v| v.as_str());
        let name = profile.get("fullName").and_then(|v| v.as_str());
        let admin = profile.get("adminRoles").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        if let Some(e) = email {
            println!("  {}  {}", "Email      ".dimmed(), e.cyan());
        }
        if let Some(n) = name {
            println!("  {}  {}", "Name       ".dimmed(), n.bold());
        }
        if admin > 0 {
            let role_names: Vec<String> = profile
                .get("adminRoles")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect())
                .unwrap_or_default();
            println!(
                "  {}  {} {}",
                "Admin roles".dimmed(),
                role_names.join(", ").yellow(),
                format!("({admin})").dimmed()
            );
        }
    }

    // Active org id + (if reachable) name + member count.
    if let Some(ref o) = creds.active_org_id {
        let mut shown = false;
        if let Ok(org) = client.get(&format!("/organizations/{o}")).await {
            let name = org.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
            let access = org.get("accessType").and_then(|v| v.as_str()).unwrap_or("");
            let access_suffix = if access.is_empty() { String::new() } else { format!(" [{access}]") };
            println!("  {}  {} {}{}", "Org        ".dimmed(), o.cyan(), name.bold(), access_suffix.dimmed());
            shown = true;
        }
        if !shown {
            println!("  {}  {}", "Org        ".dimmed(), o.cyan());
        }
        // Member count is a separate call; safe to fail.
        if let Ok(members) = client.get(&format!("/organizations/{o}/members")).await {
            let count = members
                .get("data")
                .and_then(|v| v.as_array())
                .or_else(|| members.as_array())
                .map_or(0, std::vec::Vec::len);
            if count > 0 {
                println!("  {}  {}", "Members    ".dimmed(), count.to_string().bold());
            }
        }
    } else {
        println!("  {}  {}", "Org        ".dimmed(), "(none — `th api orgs switch <id>`)".dimmed());
    }

    if let Some(exp) = creds.expires_at {
        let now = chrono::Utc::now();
        if exp > now {
            let remaining = exp - now;
            let label = if remaining.num_hours() >= 1 {
                format!("{}h", remaining.num_hours())
            } else {
                format!("{}m", remaining.num_minutes())
            };
            println!("  {}  {} {}", "Expires    ".dimmed(), label.green(), "left".dimmed());
        } else if creds.client_id.is_some() && creds.client_secret.is_some() {
            // Auto-refresh failed silently above — only land here if
            // ensure_fresh_token didn't manage to update expires_at.
            println!("  {}  {}", "Expires    ".dimmed(), "expired — auto-refresh failed, run `th api login`".red());
        } else {
            println!("  {}  {}", "Expires    ".dimmed(), "expired — `th api login`".red());
        }
    }
    println!("  {}  {}", "Stored at  ".dimmed(), client.store.path().display().to_string().dimmed());
    println!();
    Ok(())
}

/// `th orgs *` dispatch — list / show / switch.
pub async fn cmd_orgs(cmd: super::OrgsCommands) -> Result<()> {
    let client = require_authed().await?;
    match cmd {
        super::OrgsCommands::List => {
            let body = client.get("/organizations").await.context("GET /organizations")?;
            print_orgs_list(&body);
        }
        super::OrgsCommands::Show { org_id } => {
            // Use the shared resolver so `th api orgs show` honors
            // the same active-org contract as the rest of the CLI.
            let resolved =
                crate::active_org::resolve(org_id).context("no org id specified and no active org set — pass <org_id> or run `th api orgs switch <id>`")?;
            print_json(&client.get(&format!("/organizations/{resolved}")).await.context("GET /organizations/{org_id}")?);
        }
        super::OrgsCommands::Switch { org_id } => {
            // Persist to every credential store we know about so the
            // active org is visible to `th config`, `th auth whoami`,
            // and any other subcommand that reads a different store.
            // See `crates/smooth-cli/src/active_org.rs` for the
            // cross-subcommand contract this enforces.
            let updated = crate::active_org::set(&org_id).context("save active org")?;
            println!();
            println!("  {} Active org set to {}", "✓".green().bold(), org_id.cyan().bold());
            if updated > 1 {
                println!("    {} updated {} credential stores", "●".dimmed(), updated.to_string().dimmed());
            }
            println!();
        }
    }
    Ok(())
}

/// Pretty-print the org-list response. Accepts both the
/// `{data: [...]}` envelope and a bare array, because the API surface
/// is in flux.
fn print_orgs_list(body: &serde_json::Value) {
    let items = body.get("data").and_then(|v| v.as_array()).or_else(|| body.as_array());
    let Some(items) = items else {
        println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
        return;
    };
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), "No organizations".dimmed());
        return;
    }
    for org in items {
        let id = org.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = org.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        let slug = org.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        let slug_part = if slug.is_empty() { String::new() } else { format!(" ({slug})") };
        println!("  {} {} {}{}", "○".dimmed(), id.cyan(), name.bold(), slug_part.dimmed());
    }
}
