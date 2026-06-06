//! Shared active-org resolution + persistence.
//!
//! The CLI has historically had two parallel credential systems
//! that each tracked an `active_org_id` independently:
//!
//! - **legacy** `smooth_api_client::CredentialsStore::default_path()`
//!   (`~/.smooth/auth/smooai.json`) — used by `th api …` resource
//!   commands via `SmoothApiClient::from_disk()`.
//! - **client-shared** `smooai_client_shared::auth::storage::
//!   CredentialsStore::{default_m2m,default_user}()`
//!   (`~/.smooth/auth/smooai{,-user}.json`) — used by `th auth …` and
//!   `th config …` and `th admin …`.
//!
//! The two were wired to write through different code paths
//! (`SmoothApiClient::set_credentials` vs `CredentialsStore::save`),
//! which meant `th api orgs switch <id>` only updated the legacy
//! store, while `th config list` (and `th auth whoami`) read from the
//! client-shared store. Net effect: switch reported success, every
//! other subcommand kept failing with "no active org set".
//!
//! This module owns the cross-subcommand contract. Both directions
//! (read + write) go through here so the three stores never diverge.
//!
//! ## Read order — [`resolve`]
//!
//! Returns the first non-empty value found:
//!
//! 1. `override_org` (the `--org` / `--org-id` flag)
//! 2. `$SMOOAI_ORG_ID`
//! 3. legacy `smooth_api_client` store
//! 4. client-shared M2M store
//! 5. client-shared User store
//!
//! Order between (3)/(4)/(5) is "whichever has a non-empty
//! `active_org_id`"; in practice they should agree after [`set`].
//!
//! ## Write fan-out — [`set`]
//!
//! Writes `active_org_id = Some(id)` to **every** store whose
//! credentials file currently exists. Missing files are skipped (we
//! don't want `th api orgs switch` to side-effect by creating empty
//! files for sessions the user never opened).

use anyhow::{Context, Result};

/// Resolve the active org. Returns `Err` when none of the sources have
/// a usable value.
///
/// # Errors
/// - Surfaces filesystem errors from any store load
/// - Returns the standard "no active org set" message when every
///   source is empty
pub fn resolve(override_org: Option<String>) -> Result<String> {
    // 1. Explicit flag wins. Empty/whitespace doesn't count — that's
    //    how `--org=""` accidents get handled.
    if let Some(o) = override_org.filter(|s| !s.trim().is_empty()) {
        return Ok(o);
    }
    // 2. Env override (CI scripts).
    if let Ok(o) = std::env::var("SMOOAI_ORG_ID") {
        if !o.trim().is_empty() {
            return Ok(o);
        }
    }
    // 3-5. Persisted stores. Walk all three and return the first
    //      non-empty `active_org_id`. Persisted means "file exists and
    //      parses". A missing file is fine — that store just doesn't
    //      contribute.
    for source in load_all_sources() {
        if let Some(id) = source.active_org_id() {
            if !id.trim().is_empty() {
                return Ok(id);
            }
        }
    }
    anyhow::bail!("no active org set — pass `--org-id <id>`, set SMOOAI_ORG_ID, or run `th api orgs switch <id>`")
}

/// Persist `org_id` as the active org in every store whose
/// credentials file currently exists. Returns the count of stores
/// that were updated (useful for telling the user "we updated 2
/// sessions").
///
/// # Errors
/// - Surfaces filesystem errors from any store load or save
pub fn set(org_id: &str) -> Result<usize> {
    let mut updated = 0_usize;
    let trimmed = org_id.trim();
    if trimmed.is_empty() {
        anyhow::bail!("active org id cannot be empty");
    }
    for source in load_all_sources() {
        // Only write to stores that already have credentials. A
        // store with no file means "the user isn't logged in via
        // that mechanism" and we should not create a stub.
        if source.has_credentials() {
            source
                .set_active_org(trimmed)
                .with_context(|| format!("persist active org to {}", source.label()))?;
            updated += 1;
        }
    }
    Ok(updated)
}

/// All known credential sources, in resolve-precedence order.
fn load_all_sources() -> Vec<Box<dyn OrgSource>> {
    let mut sources: Vec<Box<dyn OrgSource>> = Vec::new();
    if let Ok(s) = LegacyApiClientSource::new() {
        sources.push(Box::new(s));
    }
    if let Ok(s) = ClientSharedM2mSource::new() {
        sources.push(Box::new(s));
    }
    if let Ok(s) = ClientSharedUserSource::new() {
        sources.push(Box::new(s));
    }
    sources
}

/// Abstraction over a credentials file that can hold an
/// `active_org_id`. Keeps the resolve / set logic store-agnostic.
trait OrgSource {
    /// Display label, used in error messages.
    fn label(&self) -> &str;
    /// `true` when the underlying file exists + parses.
    fn has_credentials(&self) -> bool;
    /// Snapshot of the persisted `active_org_id`, if any.
    fn active_org_id(&self) -> Option<String>;
    /// Write `org_id` as the active org. Requires `has_credentials()`
    /// to be `true` — callers check this first.
    fn set_active_org(&self, org_id: &str) -> Result<()>;
}

// --- Legacy smooth-api-client store --------------------------------

struct LegacyApiClientSource {
    store: smooth_api_client::CredentialsStore,
}

impl LegacyApiClientSource {
    fn new() -> Result<Self> {
        Ok(Self {
            store: smooth_api_client::CredentialsStore::default_path()?,
        })
    }
}

impl OrgSource for LegacyApiClientSource {
    fn label(&self) -> &str {
        "smooth-api-client store"
    }
    fn has_credentials(&self) -> bool {
        matches!(self.store.load(), Ok(Some(_)))
    }
    fn active_org_id(&self) -> Option<String> {
        self.store.load().ok().flatten().and_then(|c| c.active_org_id)
    }
    fn set_active_org(&self, org_id: &str) -> Result<()> {
        let mut creds = self
            .store
            .load()
            .context("load legacy credentials")?
            .context("legacy credentials file missing — would create stub, refusing")?;
        creds.active_org_id = Some(org_id.to_string());
        self.store.save(&creds).context("save legacy credentials")
    }
}

// --- client-shared M2M store ---------------------------------------

struct ClientSharedM2mSource {
    store: smooai_client_shared::auth::storage::CredentialsStore,
}

impl ClientSharedM2mSource {
    fn new() -> Result<Self> {
        Ok(Self {
            store: smooai_client_shared::auth::storage::CredentialsStore::default_m2m()?,
        })
    }
}

impl OrgSource for ClientSharedM2mSource {
    fn label(&self) -> &str {
        "client-shared M2M store"
    }
    fn has_credentials(&self) -> bool {
        matches!(self.store.load(), Ok(Some(_)))
    }
    fn active_org_id(&self) -> Option<String> {
        self.store.load().ok().flatten().and_then(|c| c.active_org_id)
    }
    fn set_active_org(&self, org_id: &str) -> Result<()> {
        let mut creds = self
            .store
            .load()
            .context("load M2M credentials")?
            .context("M2M credentials file missing — would create stub, refusing")?;
        creds.active_org_id = Some(org_id.to_string());
        self.store.save(&creds).context("save M2M credentials")
    }
}

// --- client-shared User store --------------------------------------

struct ClientSharedUserSource {
    store: smooai_client_shared::auth::storage::CredentialsStore,
}

impl ClientSharedUserSource {
    fn new() -> Result<Self> {
        Ok(Self {
            store: smooai_client_shared::auth::storage::CredentialsStore::default_user()?,
        })
    }
}

impl OrgSource for ClientSharedUserSource {
    fn label(&self) -> &str {
        "client-shared User store"
    }
    fn has_credentials(&self) -> bool {
        matches!(self.store.load(), Ok(Some(_)))
    }
    fn active_org_id(&self) -> Option<String> {
        self.store.load().ok().flatten().and_then(|c| c.active_org_id)
    }
    fn set_active_org(&self, org_id: &str) -> Result<()> {
        let mut creds = self
            .store
            .load()
            .context("load User credentials")?
            .context("User credentials file missing — would create stub, refusing")?;
        creds.active_org_id = Some(org_id.to_string());
        self.store.save(&creds).context("save User credentials")
    }
}

#[cfg(test)]
mod tests {
    //! Cross-subcommand contract tests. Each test points all three
    //! credential stores at a temp dir via the documented env-var
    //! overrides (`SMOOAI_AUTH_FILE` for both legacy and client-shared
    //! M2M, `SMOOAI_USER_AUTH_FILE` for client-shared User) and then
    //! exercises [`set`] + [`resolve`] end-to-end.
    //!
    //! These are the regression bar for "if `th api orgs switch X`
    //! reports success, then `th config list` (and every other
    //! subcommand) sees X without further flags."
    //!
    //! All tests serialize on a global mutex because they mutate
    //! process-wide env vars.
    use super::*;
    use chrono::Utc;
    use smooai_client_shared::auth::storage::{CredentialKind, Credentials};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Set up a temp-dir-backed environment that points ALL three
    /// stores at distinct files inside the temp dir. Returns a guard
    /// holding the lock + tempdir so files are cleaned up after the
    /// test.
    fn setup() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        let guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().expect("tempdir");
        // SMOOAI_AUTH_FILE is honored by BOTH the legacy store AND
        // the client-shared M2M store (same env var name). That's
        // fine — they'll share the file in the test, which actually
        // matches production reality on `th api login` (both stores
        // tend to point at the same file when the user logs in via
        // the legacy path).
        std::env::set_var("SMOOAI_AUTH_FILE", tmp.path().join("smooai.json"));
        std::env::set_var("SMOOAI_USER_AUTH_FILE", tmp.path().join("smooai-user.json"));
        std::env::remove_var("SMOOAI_ORG_ID");
        (guard, tmp)
    }

    fn write_user_creds(path: &std::path::Path, active_org_id: Option<String>) {
        let creds = Credentials {
            access_token: "user-tok".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            user: Some("brent@smoo.ai".into()),
            active_org_id,
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: Utc::now(),
        };
        let store = smooai_client_shared::auth::storage::CredentialsStore::at(path);
        store.save(&creds).expect("save user creds");
    }

    fn write_m2m_creds(path: &std::path::Path, active_org_id: Option<String>) {
        let creds = smooth_api_client::Credentials {
            access_token: "m2m-tok".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            user: Some("client:bee846cc".into()),
            active_org_id,
            client_id: Some("cid".into()),
            client_secret: Some("csec".into()),
            created_at: Utc::now(),
        };
        let store = smooth_api_client::CredentialsStore::at(path);
        store.save(&creds).expect("save m2m creds");
    }

    #[test]
    fn resolve_uses_override_flag_first() {
        let (_g, _tmp) = setup();
        assert_eq!(resolve(Some("flag-org".into())).unwrap(), "flag-org");
    }

    #[test]
    fn resolve_uses_env_var_when_no_override() {
        let (_g, _tmp) = setup();
        std::env::set_var("SMOOAI_ORG_ID", "env-org");
        let got = resolve(None).unwrap();
        std::env::remove_var("SMOOAI_ORG_ID");
        assert_eq!(got, "env-org");
    }

    #[test]
    fn resolve_blank_override_is_ignored() {
        let (_g, tmp) = setup();
        write_m2m_creds(&tmp.path().join("smooai.json"), Some("file-org".into()));
        assert_eq!(resolve(Some("   ".into())).unwrap(), "file-org");
    }

    #[test]
    fn resolve_errors_when_nothing_is_set() {
        let (_g, _tmp) = setup();
        let err = resolve(None).unwrap_err();
        assert!(err.to_string().contains("no active org set"), "got: {err}");
    }

    /// The headline regression. Simulates the user's reproduction:
    /// they have an M2M session, the User session has no active org,
    /// then `th api orgs switch X` is called. After that call,
    /// `resolve(None)` MUST return X regardless of which store
    /// `th config` happens to consult.
    #[test]
    fn set_then_resolve_cross_subcommand_contract() {
        let (_g, tmp) = setup();
        // Starting state mirrors the bug report: M2M has no org, User
        // has no org (User file present but empty active_org_id).
        write_m2m_creds(&tmp.path().join("smooai.json"), None);
        write_user_creds(&tmp.path().join("smooai-user.json"), None);

        // `th api orgs switch <id>` calls into here.
        let updated = set("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e").expect("set succeeded");
        assert!(updated >= 1, "at least one store should have been updated, got {updated}");

        // `th config list` calls into here.
        let resolved = resolve(None).expect("resolve after set");
        assert_eq!(resolved, "8be5f5fd-cf71-43ba-9df9-01e15acdaf8e");
    }

    /// Fan-out: when both the User and M2M files exist, `set` writes
    /// to BOTH. This is what stops the bug from coming back through a
    /// different code path (e.g. `th config` reading the User store
    /// while `th api ...` reads M2M).
    #[test]
    fn set_writes_to_all_existing_stores() {
        let (_g, tmp) = setup();
        write_m2m_creds(&tmp.path().join("smooai.json"), None);
        write_user_creds(&tmp.path().join("smooai-user.json"), None);

        set("org-fanout").expect("set");

        // Read each store directly and confirm they now agree.
        let m2m = smooth_api_client::CredentialsStore::at(tmp.path().join("smooai.json")).load().unwrap().unwrap();
        let user = smooai_client_shared::auth::storage::CredentialsStore::at(tmp.path().join("smooai-user.json"))
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(m2m.active_org_id.as_deref(), Some("org-fanout"));
        assert_eq!(user.active_org_id.as_deref(), Some("org-fanout"));
    }

    /// `set` does not invent files for sessions the user never
    /// opened. If only the M2M file exists, only the M2M file gets
    /// written — we don't fabricate a stub User session.
    ///
    /// Note: the legacy `smooth-api-client` store and the
    /// `smooai-client-shared` M2M store both honor `SMOOAI_AUTH_FILE`,
    /// so in this test they share the same underlying file. Both
    /// "count" as updates (different stores via different code paths
    /// pointing at the same file), so we expect `updated >= 1` and
    /// the file is still there — and the User file is NOT created.
    #[test]
    fn set_skips_stores_with_no_existing_credentials() {
        let (_g, tmp) = setup();
        write_m2m_creds(&tmp.path().join("smooai.json"), None);
        // No user file written.

        let updated = set("org-only-m2m").expect("set");
        // Two stores share the M2M file path via `SMOOAI_AUTH_FILE` —
        // both legitimately "have credentials" because they both see
        // the same file. What we MUST NOT do is fabricate the user
        // file. Both `1` and `2` are acceptable here; the key
        // invariant is the user file stays absent.
        assert!(updated >= 1, "at least the m2m file should be updated, got {updated}");

        // M2M got the org.
        let m2m = smooth_api_client::CredentialsStore::at(tmp.path().join("smooai.json")).load().unwrap().unwrap();
        assert_eq!(m2m.active_org_id.as_deref(), Some("org-only-m2m"));
        // User file was NOT created.
        assert!(!tmp.path().join("smooai-user.json").exists(), "user file must not be fabricated");
    }

    /// `set` refuses an empty / whitespace-only org id rather than
    /// silently writing nonsense.
    #[test]
    fn set_rejects_empty_org_id() {
        let (_g, _tmp) = setup();
        assert!(set("").is_err());
        assert!(set("   ").is_err());
    }

    /// When the M2M store has the org and the User store has it too
    /// but they disagree (e.g. legacy switch wrote one but not the
    /// other before the fix), `resolve` returns the first one it
    /// finds — but more importantly, after a [`set`], both stores
    /// agree and the choice no longer matters.
    #[test]
    fn set_aligns_disagreeing_stores() {
        let (_g, tmp) = setup();
        write_m2m_creds(&tmp.path().join("smooai.json"), Some("old-org".into()));
        write_user_creds(&tmp.path().join("smooai-user.json"), None);

        set("new-org").expect("set");

        let m2m = smooth_api_client::CredentialsStore::at(tmp.path().join("smooai.json")).load().unwrap().unwrap();
        let user = smooai_client_shared::auth::storage::CredentialsStore::at(tmp.path().join("smooai-user.json"))
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(m2m.active_org_id.as_deref(), Some("new-org"));
        assert_eq!(user.active_org_id.as_deref(), Some("new-org"));
    }
}
