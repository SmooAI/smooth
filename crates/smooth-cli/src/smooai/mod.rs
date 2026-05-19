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
pub mod config;
pub mod jobs;
pub mod keys;
pub mod knowledge;
pub mod members;
pub mod products;
pub mod profile;
pub mod testing;

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooth_api_client::auth::{poll_until_complete, start_login};
use smooth_api_client::{CredentialsStore, SmoothApiClient};

/// Build an authenticated client or fail with the standard "run
/// `th login`" message. Every resource command starts with this.
pub fn require_authed() -> Result<SmoothApiClient> {
    let client = SmoothApiClient::from_disk().context("load credentials")?;
    if !client.is_authenticated() {
        anyhow::bail!("not logged in — run `th api login` first");
    }
    Ok(client)
}

/// Resolve the active org id. Order:
///   1. `--org` flag (the `override_org` argument)
///   2. `SMOOAI_ORG_ID` env (handy for CI scripts)
///   3. `active_org_id` from `~/.smooth/auth/smooai.json`
pub fn require_active_org(client: &SmoothApiClient, override_org: Option<String>) -> Result<String> {
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    client
        .credentials()
        .and_then(|c| c.active_org_id)
        .context("no active org set — pass `--org <id>`, set SMOOAI_ORG_ID, or run `th api orgs switch <id>`")
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
        let name = item.get("name").and_then(|v| v.as_str()).or_else(|| item.get("email").and_then(|v| v.as_str())).unwrap_or("");
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let suffix = if status.is_empty() {
            String::new()
        } else {
            format!(" [{status}]")
        };
        println!("  {} {} {}{}", "○".dimmed(), id.cyan(), name.bold(), suffix.dimmed());
    }
    println!();
}

/// `th login` — device-flow handshake. Prints the verification URL +
/// user code, blocks until the user approves in the browser, persists
/// the resulting tokens at `~/.smooth/auth/smooai.json`.
pub async fn cmd_login() -> Result<()> {
    println!();
    let base_url = smooth_api_client::base_url();
    let http = reqwest::Client::builder()
        .user_agent(format!("smooth-cli/{} (https://github.com/SmooAI/smooth)", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build http client")?;

    println!("  {} Starting device-flow against {}", "●".cyan(), base_url.dimmed());
    let start = start_login(&base_url, &http).await.context("start_login")?;

    println!();
    println!("  {}  {}", "Open  ".dimmed(), start.verification_url.cyan().bold());
    println!("  {}  {}", "Enter ".dimmed(), start.user_code.yellow().bold());
    println!();
    println!("  {}", "Waiting for approval…".dimmed());

    let creds = poll_until_complete(&base_url, &http, &start).await.context("poll_until_complete")?;
    let store = CredentialsStore::default_path()?;
    store.save(&creds).context("save credentials")?;

    println!();
    println!("  {} {}", "✓".green().bold(), "Logged in".green().bold());
    if let Some(ref u) = creds.user {
        println!("    {}  {}", "User  ".dimmed(), u.cyan());
    }
    if let Some(ref o) = creds.active_org_id {
        println!("    {}  {}", "Org   ".dimmed(), o.cyan());
    }
    println!("    {}  {}", "Saved ".dimmed(), store.path().display().to_string().dimmed());
    println!();
    Ok(())
}

/// `th logout` — delete the credentials file. Idempotent.
pub async fn cmd_logout() -> Result<()> {
    let store = CredentialsStore::default_path()?;
    let existed = store.load().context("load credentials")?.is_some();
    store.delete().context("delete credentials")?;
    println!();
    if existed {
        println!("  {} Logged out", "🔴".to_string());
    } else {
        println!("  {} {}", "●".dimmed(), "Already logged out".dimmed());
    }
    println!();
    Ok(())
}

/// `th whoami` — print user + active org from the stored credentials.
/// No network call — pure local lookup. Real "is the token still
/// valid" check requires a server endpoint we'll wire later (probably
/// `GET /profile`).
pub async fn cmd_whoami() -> Result<()> {
    let client = SmoothApiClient::from_disk().context("load credentials")?;
    let Some(creds) = client.credentials() else {
        println!();
        println!("  {} {}", "●".yellow(), "Not logged in — run `th api login`".yellow());
        println!();
        return Ok(());
    };

    println!();
    if let Some(ref u) = creds.user {
        println!("  {}  {}", "User      ".dimmed(), u.cyan().bold());
    } else {
        println!("  {}  {}", "User      ".dimmed(), "(unknown)".dimmed());
    }
    if let Some(ref o) = creds.active_org_id {
        println!("  {}  {}", "Active org".dimmed(), o.cyan());
    } else {
        println!("  {}  {}", "Active org".dimmed(), "(none — `th api orgs switch <id>`)".dimmed());
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
            println!("  {}  {} {}", "Expires   ".dimmed(), label.green(), "left".dimmed());
        } else {
            println!("  {}  {}", "Expires   ".dimmed(), "expired — `th api login`".red());
        }
    }
    println!("  {}  {}", "Stored at ".dimmed(), client.store.path().display().to_string().dimmed());
    println!();
    Ok(())
}

/// `th orgs *` dispatch — list / show / switch.
pub async fn cmd_orgs(cmd: super::OrgsCommands) -> Result<()> {
    let client = SmoothApiClient::from_disk().context("load credentials")?;
    if !client.is_authenticated() {
        println!();
        println!("  {} {}", "●".yellow(), "Not logged in — run `th api login` first".yellow());
        println!();
        anyhow::bail!("not authenticated");
    }

    let pb = client.pb();
    match cmd {
        super::OrgsCommands::List => {
            let resp = pb.get_organizations().await.context("GET /organizations")?;
            println!();
            // The response is the generated wrapper; pull the JSON
            // value out and walk it generically. We don't bind to a
            // concrete struct here because the typed Organization
            // model from progenitor has nullable fields we don't
            // want to enumerate one-by-one for pretty-printing.
            let body = serde_json::to_value(resp.into_inner()).context("serialize response")?;
            print_orgs_list(&body);
            println!();
        }
        super::OrgsCommands::Show { org_id } => {
            let resolved = org_id.or_else(|| client.credentials().and_then(|c| c.active_org_id)).context("no org id specified and no active org set — pass <org_id> or run `th api orgs switch <id>`")?;
            let resp = pb.get_organizations_org_id(&resolved).await.context("GET /organizations/{org_id}")?;
            let body = serde_json::to_value(resp.into_inner()).context("serialize response")?;
            println!();
            println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            println!();
        }
        super::OrgsCommands::Switch { org_id } => {
            let mut creds = client.credentials().context("no credentials loaded")?;
            creds.active_org_id = Some(org_id.clone());
            client.set_credentials(creds).context("save credentials")?;
            println!();
            println!("  {} Active org set to {}", "✓".green().bold(), org_id.cyan().bold());
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
        let slug_part = if slug.is_empty() {
            String::new()
        } else {
            format!(" ({slug})")
        };
        println!("  {} {} {}{}", "○".dimmed(), id.cyan(), name.bold(), slug_part.dimmed());
    }
}
