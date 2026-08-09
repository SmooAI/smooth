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

use std::sync::Arc;

use smooai_client_shared::auth::storage::CredentialsStore;
use smooth_operator_svc::auth::{AuthError, AuthVerifier, Principal, Role};
use smooth_policy::family::FamilyConfig;

/// The org id used when there is no signed-in Smoo session — same placeholder
/// the engine's `LocalTokenVerifier` always used.
pub const LOCAL_ORG: &str = "local";

/// Local-token verifier whose principal carries the signed-in Smoo org.
pub struct SmooOrgVerifier {
    secret: String,
    store: Option<CredentialsStore>,
    /// Optional family roster (ADR-008, th-12d875). When present, a bearer that
    /// isn't the owner secret but matches a family member's token authenticates as
    /// that member — a `Basic` principal carrying a `role:<role>` group the tool
    /// provider reads to narrow the tool set. Absent ⇒ single-tenant as before.
    family: Option<Arc<FamilyConfig>>,
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
            family: None,
        }
    }

    /// A verifier over an explicit credentials store. For tests.
    #[must_use]
    pub fn with_store(secret: impl Into<String>, store: CredentialsStore) -> Self {
        Self {
            secret: secret.into(),
            store: Some(store),
            family: None,
        }
    }

    /// Attach a family roster so non-owner member tokens authenticate as their
    /// role (ADR-008). `None` leaves the verifier single-tenant.
    #[must_use]
    pub fn with_family(mut self, family: Option<Arc<FamilyConfig>>) -> Self {
        self.family = family;
        self
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
        // Owner FIRST: the operator secret → Admin, unchanged. Every candidate is
        // still a full constant-time compare (no early-length leak beyond
        // `local_token_eq`), and a family token can never satisfy this branch.
        if local_token_eq(bearer_token.as_bytes(), self.secret.as_bytes()) {
            // Read per-verify, not per-construct: the heartbeat rotates the session
            // and `th api orgs switch` moves the org under a running daemon.
            let (org_id, user_id, display_name) = self.identity();
            tracing::debug!(%org_id, %user_id, "chat connection authenticated (owner)");
            // Single-user local daemon — the operator is always their own admin.
            return Ok(Principal::new(user_id, org_id, Role::Admin, display_name));
        }
        // Then family (ADR-008, th-12d875): a member token → a scoped principal
        // carrying `role:<role>` in `groups`, which the tool provider reads to
        // narrow the tool set. The engine drops `Principal.role` at connect
        // (`access_context()`), so the ROLE MUST ride in `groups`, not the role
        // field. Family members share the owner's org (they're one Smoo family).
        if let Some(family) = &self.family {
            if let Some(member) = family.member_for_token(bearer_token) {
                let (org_id, _, _) = self.identity();
                tracing::debug!(%org_id, member = %member.id, role = %member.role, "chat connection authenticated (family member)");
                let mut principal = Principal::new(member.id.clone(), org_id, Role::Basic, member.display_name.clone());
                principal.groups = vec![format!("role:{}", member.role)];
                return Ok(principal);
            }
        }
        Err(AuthError::InvalidToken("local token mismatch".to_string()))
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

    fn family(toml: &str) -> std::sync::Arc<FamilyConfig> {
        std::sync::Arc::new(FamilyConfig::from_toml(toml).unwrap())
    }

    const FAM: &str = r#"
        [[members]]
        token = "jr-token-aaaa"
        id = "kid-alex"
        role = "child"
        display_name = "Alex"

        [roles.child]
        allow = ["read_file"]
        default = "deny"
    "#;

    #[test]
    fn family_token_authenticates_as_a_scoped_role() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("fam-org"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("owner-secret", store).with_family(Some(family(FAM)));

        let p = v.verify("jr-token-aaaa").expect("family token authenticates");
        assert_eq!(p.user_id, "kid-alex");
        assert_eq!(p.display_name.as_deref(), Some("Alex"));
        // Role rides in groups (the engine drops the role field), and the member
        // shares the owner's org.
        assert_eq!(p.groups, vec!["role:child".to_string()]);
        assert_eq!(p.org_id, "fam-org");
        assert_eq!(p.role, Role::Basic, "a family member is never Admin");
    }

    #[test]
    fn owner_token_is_unchanged_by_a_family_roster() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("fam-org"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("owner-secret", store).with_family(Some(family(FAM)));

        let p = v.verify("owner-secret").expect("owner token still works");
        assert_eq!(p.role, Role::Admin);
        assert!(p.groups.is_empty(), "owner carries no role group → unfiltered tools");
    }

    #[test]
    fn no_role_escalation_from_a_family_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("fam-org"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("owner-secret", store).with_family(Some(family(FAM)));

        // A family token can never satisfy the owner branch (distinct bytes), and a
        // prefix/altered owner token is rejected outright — never downgraded to a
        // family match either.
        assert_eq!(v.verify("jr-token-aaaa").unwrap().role, Role::Basic);
        assert!(matches!(v.verify("owner"), Err(AuthError::InvalidToken(_))));
        assert!(matches!(v.verify("jr-token"), Err(AuthError::InvalidToken(_))), "member-token prefix must miss");
    }

    #[test]
    fn unknown_token_fails_closed_even_with_a_family() {
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("fam-org"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("owner-secret", store).with_family(Some(family(FAM)));

        assert!(matches!(v.verify(""), Err(AuthError::Unauthenticated)));
        assert!(matches!(v.verify("stranger"), Err(AuthError::InvalidToken(_))));
    }

    #[test]
    fn without_a_family_behavior_is_byte_for_byte_as_before() {
        // No family attached: owner works, everything else is InvalidToken —
        // identical to the pre-ADR-008 verifier.
        let dir = tempfile::tempdir().unwrap();
        let store = tmp_store(&dir);
        store.save(&creds(Some("org"), Some("dev@smoo.ai"))).unwrap();
        let v = SmooOrgVerifier::with_store("owner-secret", store);
        assert_eq!(v.verify("owner-secret").unwrap().role, Role::Admin);
        assert!(matches!(v.verify("jr-token-aaaa"), Err(AuthError::InvalidToken(_))));
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
