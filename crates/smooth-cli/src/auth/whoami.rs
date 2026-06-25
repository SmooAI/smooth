//! `th auth whoami` — show every stored session and its state.
//!
//! Before reporting, attempt a silent refresh for any expired session
//! that has the necessary refresh material (Supabase `refresh_token`
//! for the user JWT, stored `client_id` / `client_secret` for the M2M
//! grant). If the refresh succeeds, persist the new credentials and
//! report the fresh state; otherwise fall back to reporting the
//! still-expired state. This way the user sees the truth the next
//! `th config` / `th api` call would see, instead of being told to
//! "log in again" when nothing of the sort is required.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};

use crate::auth::refresh::{refresh_m2m_session, refresh_user_session};

/// Print both the user and the M2M sessions if present. The two are
/// independent — having one doesn't imply having the other.
pub async fn cmd_whoami() -> Result<()> {
    let user = CredentialsStore::default_user().context("locate user store")?;
    let m2m = CredentialsStore::default_m2m().context("locate M2M store")?;

    let user_creds = user.load().context("load user session")?;
    let m2m_creds = m2m.load().context("load M2M session")?;

    if user_creds.is_none() && m2m_creds.is_none() {
        println!("{} Not logged in. Run {} to start.", "—".dimmed(), "th auth login".bold());
        return Ok(());
    }

    let http = reqwest::Client::builder()
        .user_agent(format!("smooth-cli/{} (https://github.com/SmooAI/smooth)", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build http client")?;

    // Silent refresh per session. A failure here just means we fall
    // through to reporting the still-expired creds — we don't surface
    // refresh errors to the user from `whoami`, since they may be
    // running it diagnostically and don't want their terminal flooded
    // with network errors. The next command that actually needs the
    // session will surface a real error if/when it tries to use it.
    let user_creds = if let Some(creds) = user_creds {
        if creds.is_expired() && creds.refresh_token.is_some() {
            match refresh_user_session(&http, &creds).await {
                Ok(refreshed) => {
                    let _ = user.save(&refreshed);
                    Some(refreshed)
                }
                Err(_) => Some(creds),
            }
        } else {
            Some(creds)
        }
    } else {
        None
    };
    let m2m_creds = if let Some(creds) = m2m_creds {
        if creds.is_expired() && creds.client_id.is_some() && creds.client_secret.is_some() {
            match refresh_m2m_session(&http, &creds).await {
                Ok(refreshed) => {
                    let _ = m2m.save(&refreshed);
                    Some(refreshed)
                }
                Err(_) => Some(creds),
            }
        } else {
            Some(creds)
        }
    } else {
        None
    };

    if let Some(creds) = &user_creds {
        print_session("User", creds, user.path());
    }
    if let Some(creds) = &m2m_creds {
        if user_creds.is_some() {
            println!();
        }
        print_session("M2M", creds, m2m.path());
    }
    // Discoverability nudge: the active org is switchable. Only worth
    // showing for the user JWT — M2M tokens are org-locked server-side,
    // so switching is cosmetic there. `th org` is the top-level alias
    // for `th api orgs`.
    if user_creds.is_some() {
        println!();
        println!(
            "  {} {} to list orgs, {} to change the active org (user JWT acts cross-org; M2M is org-locked).",
            "→".dimmed(),
            "th org list".bold(),
            "th org switch <id|name>".bold(),
        );
    }
    Ok(())
}

fn print_session(label: &str, creds: &Credentials, path: &std::path::Path) {
    println!("{} {}", format!("{label}:").bold().cyan(), creds.user.as_deref().unwrap_or("(unknown)"));
    if let Some(exp) = creds.expires_at {
        let exp_str = exp.format("%Y-%m-%d %H:%M UTC").to_string();
        if creds.is_expired() {
            println!("  {} {} {}", "expires".dimmed(), exp_str.red().bold(), "(expired — log in again)".dimmed());
        } else {
            println!("  {} {}", "expires".dimmed(), exp_str);
        }
    } else {
        println!("  {} {}", "expires".dimmed(), "(no expiry)".dimmed());
    }
    if let Some(org) = &creds.active_org_id {
        println!("  {} {}", "active org".dimmed(), org);
    }
    println!("  {} {}", "file".dimmed(), path.display().to_string().dimmed());
}
