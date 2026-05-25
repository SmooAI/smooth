//! `th auth logout` — clear one or both stored sessions.

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use smooai_client_shared::auth::storage::CredentialsStore;

/// Clear the user session (default), M2M session (`--m2m`), or both
/// (`--all`).
pub fn cmd_logout(m2m: bool, all: bool) -> Result<()> {
    if all {
        let user = CredentialsStore::default_user().context("locate user store")?;
        let m2m = CredentialsStore::default_m2m().context("locate M2M store")?;
        user.delete().context("delete user session")?;
        m2m.delete().context("delete M2M session")?;
        println!("{} Cleared both user and M2M sessions.", "✓".green().bold());
        return Ok(());
    }
    if m2m {
        let store = CredentialsStore::default_m2m().context("locate M2M store")?;
        store.delete().context("delete M2M session")?;
        println!("{} M2M session cleared (user session untouched).", "✓".green().bold());
    } else {
        let store = CredentialsStore::default_user().context("locate user store")?;
        store.delete().context("delete user session")?;
        println!("{} User session cleared (M2M session untouched).", "✓".green().bold());
    }
    Ok(())
}
