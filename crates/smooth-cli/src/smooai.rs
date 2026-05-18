//! Smoo AI platform CLI commands — `th login` / `logout` / `whoami` /
//! `orgs *`. All go through `smooth_api_client` against `api.smoo.ai`.
//!
//! Output uses the same `colored` palette + spacing the rest of the
//! CLI uses, so help / status / login share a look.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooth_api_client::auth::{poll_until_complete, start_login};
use smooth_api_client::{CredentialsStore, SmoothApiClient};

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
        println!("  {} {}", "●".yellow(), "Not logged in — run `th login`".yellow());
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
        println!("  {}  {}", "Active org".dimmed(), "(none — `th orgs switch <id>`)".dimmed());
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
            println!("  {}  {}", "Expires   ".dimmed(), "expired — `th login`".red());
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
        println!("  {} {}", "●".yellow(), "Not logged in — run `th login` first".yellow());
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
            let resolved = org_id.or_else(|| client.credentials().and_then(|c| c.active_org_id)).context("no org id specified and no active org set — pass <org_id> or run `th orgs switch <id>`")?;
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
