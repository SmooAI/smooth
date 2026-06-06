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
    let creds = store.load().context("load user credentials")?;
    Ok(creds.and_then(|c| c.active_org_id))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;
    use smooai_client_shared::auth::storage::{CredentialKind, Credentials, CredentialsStore};
    use tempfile::TempDir;

    use super::*;

    /// Tests in this module flip the `SMOOAI_AUTH_FILE` env var so they
    /// hit a tempdir instead of the user's real `~/.smooth/auth/`.
    /// They mutate process-global state, so run them under a mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Stash + restore an env var across a test.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
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
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _g = EnvGuard::set("SMOOAI_USER_AUTH_FILE", path.to_str().unwrap());

        // Save a baseline user session with no active org.
        let store = CredentialsStore::default_user().expect("locate store");
        store.save(&fixture_creds()).expect("save baseline");

        set("org_42").expect("set active org");

        let loaded = resolve().expect("resolve").expect("some org");
        assert_eq!(loaded, "org_42");
    }

    #[test]
    fn set_overwrites_existing_active_org_id() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _g = EnvGuard::set("SMOOAI_USER_AUTH_FILE", path.to_str().unwrap());

        let mut creds = fixture_creds();
        creds.active_org_id = Some("org_old".into());
        CredentialsStore::default_user().expect("store").save(&creds).expect("save");

        set("org_new").expect("set");
        assert_eq!(resolve().expect("resolve").as_deref(), Some("org_new"));
    }

    #[test]
    fn set_errors_when_no_user_session_present() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _g = EnvGuard::set("SMOOAI_USER_AUTH_FILE", path.to_str().unwrap());

        // No credentials saved → cannot persist active org.
        let err = set("org_x").expect_err("expected bail");
        let msg = format!("{err:#}");
        assert!(msg.contains("no user credentials loaded"), "got: {msg}");
    }

    #[test]
    fn resolve_returns_none_when_store_is_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _g = EnvGuard::set("SMOOAI_USER_AUTH_FILE", path.to_str().unwrap());

        assert!(resolve().expect("resolve").is_none());
    }

    #[test]
    fn resolve_returns_none_when_active_org_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _g = EnvGuard::set("SMOOAI_USER_AUTH_FILE", path.to_str().unwrap());

        CredentialsStore::default_user().expect("store").save(&fixture_creds()).expect("save");
        assert!(resolve().expect("resolve").is_none());
    }
}
