//! `th auth whoami` — show every stored session and its state.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::storage::{Credentials, CredentialsStore};

/// Print both the user and the M2M sessions if present. The two are
/// independent — having one doesn't imply having the other.
pub fn cmd_whoami() -> Result<()> {
    let user = CredentialsStore::default_user().context("locate user store")?;
    let m2m = CredentialsStore::default_m2m().context("locate M2M store")?;

    let user_creds = user.load().context("load user session")?;
    let m2m_creds = m2m.load().context("load M2M session")?;

    if user_creds.is_none() && m2m_creds.is_none() {
        println!("{} Not logged in. Run {} to start.", "—".dimmed(), "th auth login".bold());
        return Ok(());
    }

    if let Some(creds) = &user_creds {
        print_session("User", creds, user.path());
    }
    if let Some(creds) = &m2m_creds {
        if user_creds.is_some() {
            println!();
        }
        print_session("M2M", creds, m2m.path());
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
