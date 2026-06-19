//! Shared writer for the "currently active org" state.
//!
//! Pearl th-3217db is landing a cross-store writer that updates **all**
//! credential stores in lockstep (user JWT, M2M, profile). Until that
//! lands, this module is the focused single-store integration point the
//! browser-login flow needs: it persists `active_org_id` onto the user
//! credentials file at `~/.smooth/auth/smooai-user.json` so that
//! follow-on commands (`th api orgs show`, `th config list`, …) pick
//! up the org the user just chose in the browser without a second
//! `th api orgs switch`.
//!
//! When th-3217db ships, the implementation of [`set`] swaps to its
//! cross-store writer; callers don't change.
//!
//! Pearl th-fcb579.
//!
//! # Deviation from DESIGN.md
//!
//! DESIGN.md references `active_org::set()` "from th-3217db" as if it
//! already exists. It does not — th-3217db hasn't landed at the time
//! of this commit. The reasonable call is to introduce a tiny
//! single-store writer with the same signature and door for the
//! cross-store rewrite, so the browser-login flow can ship now and
//! th-3217db swaps the implementation transparently later.

use anyhow::{Context, Result};
use smooai_client_shared::auth::storage::CredentialsStore;

/// Persist `org_id` as the active org on the user credentials store
/// (`~/.smooth/auth/smooai-user.json`).
///
/// No-ops if the user store is empty — the caller is responsible for
/// having just saved fresh credentials before invoking this.
///
/// # Errors
/// - Locating the credentials store fails
/// - Reading the credentials file fails
/// - Writing the credentials file fails
///
/// # When th-3217db lands
/// Replace the body of this function with the cross-store writer; the
/// signature stays the same.
pub fn set(org_id: &str) -> Result<()> {
    let store = CredentialsStore::default_user().context("locate user credentials store")?;
    set_in(&store, org_id)
}

/// Store-injected core of [`set`]. Lets tests pass an explicit
/// `CredentialsStore::at(<tempfile>)` instead of mutating the
/// process-global `SMOOAI_USER_AUTH_FILE` env var — which made these
/// tests race against the cross-store `active_org` module's tests
/// (separate locks, same env var) and flake the Release run. Pearl
/// th-2944e5.
fn set_in(store: &CredentialsStore, org_id: &str) -> Result<()> {
    let Some(mut creds) = store.load().context("load user credentials")? else {
        // No user credentials = no active-org slot to write to. The
        // browser-login caller always saves credentials first; if we
        // reach this branch, the caller's contract is broken — surface
        // it loudly so the bug doesn't silently swallow the user's
        // org choice.
        anyhow::bail!("no user credentials loaded; cannot set active org");
    };
    creds.active_org_id = Some(org_id.to_string());
    store.save(&creds).context("persist active_org_id")?;
    Ok(())
}

/// Read the currently-persisted active org id off the user credentials
/// store, returning `None` if no credentials are present or the field
/// is unset. Used by the browser-login integration test to verify the
/// write contract end-to-end.
///
/// # Errors
/// - Locating or reading the credentials store fails
pub fn resolve() -> Result<Option<String>> {
    let store = CredentialsStore::default_user().context("locate user credentials store")?;
    resolve_in(&store)
}

/// Store-injected core of [`resolve`] (see [`set_in`] for why).
fn resolve_in(store: &CredentialsStore) -> Result<Option<String>> {
    let creds = store.load().context("load user credentials")?;
    Ok(creds.and_then(|c| c.active_org_id))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use smooai_client_shared::auth::storage::{CredentialKind, Credentials, CredentialsStore};
    use tempfile::TempDir;

    use super::*;

    /// Hermetic store backed by a tempfile — `CredentialsStore::at` takes
    /// an explicit path, so these tests never touch the process-global
    /// `SMOOAI_USER_AUTH_FILE` env var or the user's real
    /// `~/.smooth/auth/`. That's the fix for pearl th-2944e5: the old
    /// env-var approach raced against the cross-store `active_org`
    /// module's tests (separate locks, same global var), flaking the
    /// Release run. No env mutation → no lock needed → parallel-safe.
    fn tmp_store() -> (TempDir, CredentialsStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = CredentialsStore::at(dir.path().join("smooai-user.json"));
        (dir, store)
    }

    fn fixture_creds() -> Credentials {
        Credentials {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: None,
            user: Some("hi@example.com".into()),
            active_org_id: None,
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn set_persists_active_org_id_on_user_store() {
        let (_dir, store) = tmp_store();
        // Save a baseline user session with no active org.
        store.save(&fixture_creds()).expect("save baseline");

        set_in(&store, "org_42").expect("set active org");

        let loaded = resolve_in(&store).expect("resolve").expect("some org");
        assert_eq!(loaded, "org_42");
    }

    #[test]
    fn set_overwrites_existing_active_org_id() {
        let (_dir, store) = tmp_store();
        let mut creds = fixture_creds();
        creds.active_org_id = Some("org_old".into());
        store.save(&creds).expect("save");

        set_in(&store, "org_new").expect("set");
        assert_eq!(resolve_in(&store).expect("resolve").as_deref(), Some("org_new"));
    }

    #[test]
    fn set_errors_when_no_user_session_present() {
        let (_dir, store) = tmp_store();
        // No credentials saved → cannot persist active org.
        let err = set_in(&store, "org_x").expect_err("expected bail");
        let msg = format!("{err:#}");
        assert!(msg.contains("no user credentials loaded"), "got: {msg}");
    }

    #[test]
    fn resolve_returns_none_when_store_is_empty() {
        let (_dir, store) = tmp_store();
        assert!(resolve_in(&store).expect("resolve").is_none());
    }

    #[test]
    fn resolve_returns_none_when_active_org_unset() {
        let (_dir, store) = tmp_store();
        store.save(&fixture_creds()).expect("save");
        assert!(resolve_in(&store).expect("resolve").is_none());
    }

    /// The public `set`/`resolve` wrappers still compile + wire through
    /// `default_user()`; covered indirectly by the store-injected tests
    /// above plus the cross-store contract tests in `crate::active_org`.
    #[test]
    fn public_wrappers_exist() {
        // Reference the items so the wrappers can't be dead-code-eliminated
        // away without a compile error here.
        let _set: fn(&str) -> Result<()> = set;
        let _resolve: fn() -> Result<Option<String>> = resolve;
    }
}
