//! `th auth profile …` — manage named auth profiles. SMOODEV-1739.

use anyhow::Result;
use owo_colors::OwoColorize;
use smooai_client_shared::auth::storage::CredentialsStore;

use super::paths;
use super::ProfileCommands;

pub fn dispatch(cmd: ProfileCommands) -> Result<()> {
    match cmd {
        ProfileCommands::List => list(),
        ProfileCommands::Use { name } => {
            paths::set_active(&name)?;
            println!();
            println!("  {} active profile → {}", "✓".green().bold(), name.cyan().bold());
            println!();
            Ok(())
        }
        ProfileCommands::Rm { name } => {
            paths::remove_profile(&name)?;
            println!();
            println!("  {} removed profile {}", "✓".green().bold(), name.cyan().bold());
            println!();
            Ok(())
        }
    }
}

/// Identity behind a profile's user session, best-effort.
fn identity_of(profile: Option<&str>) -> String {
    let store = CredentialsStore::at(paths::user_file(profile));
    match store.load() {
        Ok(Some(c)) => {
            let expired = c.is_expired();
            let who = c.user.unwrap_or_else(|| "(user session)".to_string());
            if expired {
                format!("{who} (expired)")
            } else {
                who
            }
        }
        _ => {
            // No user session — note whether an M2M one exists.
            let m2m = CredentialsStore::at(paths::m2m_file(profile));
            match m2m.load() {
                Ok(Some(_)) => "(M2M only)".to_string(),
                _ => "(no session)".to_string(),
            }
        }
    }
}

fn list() -> Result<()> {
    let active = paths::active_profile();
    let named = paths::list_profiles();

    println!();
    if named.is_empty() && !paths::default_profile_present() {
        println!("  {} {}", "●".dimmed(), "no profiles yet — `th auth login --profile <name>`".dimmed());
        println!();
        return Ok(());
    }

    // Default (unnamed) profile, if it holds anything and no active override.
    if paths::default_profile_present() {
        let marker = if active.is_none() { "→".green().to_string() } else { " ".to_string() };
        println!("  {} {}  {}", marker, "default".bold(), identity_of(None).dimmed());
    }

    for name in named {
        let is_active = active.as_deref() == Some(name.as_str());
        let marker = if is_active { "→".green().to_string() } else { " ".to_string() };
        println!("  {} {}  {}", marker, name.cyan().bold(), identity_of(Some(&name)).dimmed());
    }

    println!();
    println!("  {} {}", "→".green(), "= active. Switch with `th auth profile use <name>`.".dimmed());
    println!();
    Ok(())
}
