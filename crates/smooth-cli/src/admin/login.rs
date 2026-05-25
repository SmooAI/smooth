//! `th admin login` / `logout` / `whoami`.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::oauth::{login as oauth_login, OAuthConfig};
use smooai_client_shared::auth::storage::CredentialsStore;

/// Drive the Supabase OAuth flow and persist the session to
/// `~/.smooth/auth/smooai-user.json`.
///
/// # Errors
/// Bubbles up errors from the underlying [`oauth_login`] call —
/// network failures, browser-callback timeout, non-2xx from
/// Supabase token endpoint, malformed response.
pub async fn cmd_login(provider: Option<String>) -> Result<()> {
    let url = super::supabase_url();
    let key = super::supabase_anon_key();
    let mut cfg = OAuthConfig::new(url.clone(), key);
    if let Some(p) = provider {
        cfg = cfg.with_provider(p);
    }

    println!("{} Opening browser for Smoo AI login ({})...", "→".cyan().bold(), url.dimmed());
    let http = reqwest::Client::new();
    let creds = oauth_login(&http, &cfg).await.context("Supabase OAuth login")?;

    let store = CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?;
    store.save(&creds).context("persist credentials")?;

    let user = creds.user.as_deref().unwrap_or("(unknown user)");
    println!(
        "{} Logged in as {} — session saved to {}",
        "✓".green().bold(),
        user.bold(),
        store.path().display().to_string().dimmed()
    );
    if let Some(exp) = creds.expires_at {
        println!("  {} Expires at {}", "ℹ".dimmed(), exp.format("%Y-%m-%d %H:%M UTC").to_string().dimmed());
    }
    println!();
    println!("  Run {} to see your session.", "th admin whoami".bold());
    println!("  Run {} to log out.", "th admin logout".bold());
    Ok(())
}

/// Delete the user session file. Idempotent — silently succeeds if
/// no session is present.
///
/// # Errors
/// IO failures other than "not found" bubble up.
pub fn cmd_logout() -> Result<()> {
    let store = CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?;
    store.delete().context("delete user session")?;
    println!("{} Logged out (any persisted session has been removed).", "✓".green().bold());
    Ok(())
}

/// Print the currently-logged-in user, if any.
///
/// # Errors
/// Surfaces IO / parse failures on the session file. Missing-file is
/// the "logged out" state and prints a hint, not an error.
pub fn cmd_whoami() -> Result<()> {
    let store = CredentialsStore::default_user().context("locate ~/.smooth/auth/smooai-user.json")?;
    let Some(creds) = store.load().context("load user session")? else {
        println!("{} Not logged in. Run {} to start.", "—".dimmed(), "th admin login".bold());
        return Ok(());
    };
    let user = creds.user.as_deref().unwrap_or("(unknown user)");
    println!("{} {}", "User:".dimmed(), user.bold());
    if let Some(exp) = creds.expires_at {
        let expired = creds.is_expired();
        let exp_str = exp.format("%Y-%m-%d %H:%M UTC").to_string();
        if expired {
            println!(
                "{} {} {}",
                "Expires:".dimmed(),
                exp_str.red().bold(),
                "(expired — run `th admin login` again)".dimmed()
            );
        } else {
            println!("{} {}", "Expires:".dimmed(), exp_str);
        }
    }
    println!("{} {}", "Session file:".dimmed(), store.path().display().to_string().dimmed());
    Ok(())
}
