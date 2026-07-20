//! The daemon's local-token verifier, stamped with the operator's **real**
//! Smoo AI org (th-0c63cc).
//!
//! The engine's [`LocalTokenVerifier`](smooth_operator_svc::auth::LocalTokenVerifier)
//! hardcodes `Principal::new("local", "local", …)`, so every chat connection ran
//! with `org_id = "local"` — a placeholder no org-scoped tool (web search,
//! knowledge, scraping) can do anything with, even though the daemon holds a
//! perfectly good signed-in Smoo session next door (`th auth login`, kept fresh
//! by the credential heartbeat in [`crate::auth_login`]). The two auth systems
//! simply never met.
//!
//! [`SmooOrgVerifier`] is the same local-token gate — identical constant-time
//! compare, identical fail-closed behavior — that reads `active_org_id` from the
//! stored user credentials **on every `verify()` call** (the heartbeat rotates
//! the session and the user can switch orgs; a startup-cached org would need a
//! daemon restart to notice). Logged out, unreadable, or no org → falls back to
//! `"local"`, so the signed-out UX is exactly what it is today.

use smooai_client_shared::auth::storage::CredentialsStore;
use smooth_operator_svc::auth::{AuthError, AuthVerifier, Principal, Role};

/// The org id used when there is no signed-in Smoo session — same placeholder
/// the engine's `LocalTokenVerifier` always used.
pub const LOCAL_ORG: &str = "local";

/// Local-token verifier whose principal carries the signed-in Smoo org.
pub struct SmooOrgVerifier {
    secret: String,
    store: Option<CredentialsStore>,
}

impl SmooOrgVerifier {
    /// A verifier over `secret`, reading org identity from the default user
    /// credentials file (`~/.smooth/auth/smooai-user.json`). A store that can't
    /// be located (no `$HOME`) degrades to the `"local"` fallback rather than
    /// refusing to boot — the daemon must still serve chat when signed out.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
            store: CredentialsStore::default_user().ok(),
        }
    }

    /// A verifier over an explicit credentials store. For tests.
    #[must_use]
    pub fn with_store(secret: impl Into<String>, store: CredentialsStore) -> Self {
        Self {
            secret: secret.into(),
            store: Some(store),
        }
    }

    /// `(org_id, user_id, display_name)` from the stored session, read fresh.
    fn identity(&self) -> (String, String, Option<String>) {
        let creds = self.store.as_ref().and_then(|s| s.load().ok().flatten());
        let Some(creds) = creds else {
            return (LOCAL_ORG.to_string(), LOCAL_ORG.to_string(), Some("Local user".to_string()));
        };
        let org = creds.active_org_id.filter(|o| !o.trim().is_empty()).unwrap_or_else(|| LOCAL_ORG.to_string());
        let user = creds.user.filter(|u| !u.trim().is_empty());
        (
            org,
            user.clone().unwrap_or_else(|| LOCAL_ORG.to_string()),
            user.or_else(|| Some("Local user".to_string())),
        )
    }
}

impl AuthVerifier for SmooOrgVerifier {
    fn verify(&self, bearer_token: &str) -> Result<Principal, AuthError> {
        if bearer_token.is_empty() {
            return Err(AuthError::Unauthenticated);
        }
        if !local_token_eq(bearer_token.as_bytes(), self.secret.as_bytes()) {
            return Err(AuthError::InvalidToken("local token mismatch".to_string()));
        }
        // Read per-verify, not per-construct: the heartbeat rotates the session
        // and `th api orgs switch` moves the org under a running daemon.
        let (org_id, user_id, display_name) = self.identity();
        tracing::debug!(%org_id, %user_id, "chat connection authenticated");
        // Single-user local daemon — the operator is always their own admin.
        Ok(Principal::new(user_id, org_id, Role::Admin, display_name))
    }

    fn mode(&self) -> &'static str {
        "local-token-smoo-org"
    }
}

/// Length-aware constant-time byte comparison, so the local-token check leaks
/// neither length nor content through timing. (Mirrors the engine's
/// `local_token_eq`, which is private.)
fn local_token_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooai_client_shared::auth::storage::{CredentialKind, Credentials};

    fn creds(org: Option<&str>, user: Option<&str>) -> Credentials {
        Credentials {
            access_token: "AT".into(),
            refresh_token: None,
            expires_at: None,
            user: user.map(Into::into),
            active_org_id: org.map(Into::into),
            client_id: None,
            client_secret: None,
            kind: CredentialKind::User,
            created_at: chrono::Utc::now(),
        }
    }

    /// Store rooted in a temp dir — never touches the developer's `~/.smooth`.
    fn tmp_store(dir: &tempfile::TempDir) -> CredentialsStore {
        CredentialsStore::at(dir.path().join("smooai-user.json"))
    }

    #[test]
    fn signed_in_principal_carries_the_real_org() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e"), Some("dev@smoo.ai"))).unwrap();

        let p = SmooOrgVerifier::with_store("tok", store).verify("tok").expect("matching token");
        assert_eq!(p.org_id, "8be5f5fd-cf71-43ba-9df9-01e15acdaf8e");
        assert_eq!(p.user_id, "dev@smoo.ai");
        assert_eq!(p.display_name.as_deref(), Some("dev@smoo.ai"));
        assert_eq!(p.role, Role::Admin);
    }

    #[test]
    fn signed_out_falls_back_to_local() {
        let dir = tempfile::tempdir().unwrap();
        // No credentials file at all — the logged-out state.
        let p = SmooOrgVerifier::with_store("tok", tmp_store(&dir)).verify("tok").expect("matching token");
        assert_eq!(p.org_id, LOCAL_ORG);
        assert_eq!(p.user_id, LOCAL_ORG);
        assert_eq!(p.role, Role::Admin);
    }

    #[test]
    fn credentials_without_org_fall_back_to_local() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        // Signed in, but no org picked — and the blank-string variant too.
        store.save(&creds(None, Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("tok", store.clone());
        assert_eq!(v.verify("tok").unwrap().org_id, LOCAL_ORG);

        store.save(&creds(Some("   "), Some("dev@smoo.ai"))).unwrap();
        assert_eq!(v.verify("tok").unwrap().org_id, LOCAL_ORG);
    }

    #[test]
    fn unreadable_credentials_fall_back_to_local() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smooai-user.json");
        std::fs::write(&path, "{ not json").unwrap();
        let p = SmooOrgVerifier::with_store("tok", CredentialsStore::at(path))
            .verify("tok")
            .expect("matching token");
        assert_eq!(p.org_id, LOCAL_ORG);
    }

    #[test]
    fn org_is_reread_on_every_verify() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("org-one"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("tok", store.clone());
        assert_eq!(v.verify("tok").unwrap().org_id, "org-one");

        // `th api orgs switch` (or the heartbeat) rewrites the file under a
        // running daemon — the next connection must see the new org without a
        // restart.
        store.save(&creds(Some("org-two"), Some("dev@smoo.ai"))).unwrap();
        assert_eq!(v.verify("tok").unwrap().org_id, "org-two");
    }

    #[test]
    fn wrong_and_empty_tokens_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("org-one"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("s3cret-local", store);

        assert!(matches!(v.verify(""), Err(AuthError::Unauthenticated)));
        assert!(matches!(v.verify("nope"), Err(AuthError::InvalidToken(_))));
        // Prefix of the secret — length-aware compare must reject it.
        assert!(matches!(v.verify("s3cret"), Err(AuthError::InvalidToken(_))));
        // Longer than the secret.
        assert!(matches!(v.verify("s3cret-local-extra"), Err(AuthError::InvalidToken(_))));
    }

    #[test]
    fn mode_is_a_non_secret_label() {
        let v = SmooOrgVerifier::new("s3cret-local");
        assert_eq!(v.mode(), "local-token-smoo-org");
        assert!(!v.mode().contains("s3cret"));
    }

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        assert!(local_token_eq(b"abc", b"abc"));
        assert!(!local_token_eq(b"abc", b"abd"));
        assert!(!local_token_eq(b"abc", b"ab"));
        assert!(!local_token_eq(b"", b"a"));
        assert!(local_token_eq(b"", b""));
    }
}
