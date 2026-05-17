//! Backend registry — routes `IssueCredential` requests to the
//! appropriate `Backend` by matching `server_url` against the
//! backend's declared globs.

use std::sync::Arc;

use crate::backend::{glob_matches, Backend, BackendError, BackendInfo, CredentialRequest, IssuedCredential};

/// Holds the configured backends. Cheap to clone — backends live
/// behind `Arc`.
#[derive(Clone, Default)]
pub struct BackendRegistry {
    backends: Vec<Arc<dyn Backend>>,
}

impl BackendRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new backend. Order matters — earlier backends
    /// win when their globs overlap.
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backends.push(backend);
        self
    }

    /// Number of registered backends.
    #[must_use]
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// True when no backends are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// Snapshot of every registered backend's info, in
    /// registration order.
    #[must_use]
    pub fn list_backends(&self) -> Vec<BackendInfo> {
        self.backends.iter().map(|b| b.info()).collect()
    }

    /// Find the first backend whose globs match `server_url`.
    /// Returns `None` if no backend matches.
    #[must_use]
    pub fn route(&self, server_url: &str) -> Option<Arc<dyn Backend>> {
        self.backends
            .iter()
            .find(|b| b.info().server_globs.iter().any(|g| glob_matches(g, server_url)))
            .cloned()
    }

    /// Mint a credential for `request.server_url`. Validates the
    /// URL is non-empty, finds the matching backend, checks
    /// readiness, then delegates.
    ///
    /// # Errors
    ///
    /// * `InvalidServerUrl` — empty / unparseable URL.
    /// * `NoBackend` — no backend's globs match the URL.
    /// * `NotReady` — matched backend is not currently usable
    ///   (CLI missing, user logged out).
    /// * `Mint` — backend's `issue` returned an error.
    pub async fn issue(&self, request: &CredentialRequest) -> Result<IssuedCredential, BackendError> {
        if request.server_url.trim().is_empty() {
            return Err(BackendError::InvalidServerUrl(request.server_url.clone()));
        }
        let backend = self
            .route(&request.server_url)
            .ok_or_else(|| BackendError::NoBackend(request.server_url.clone()))?;
        let info = backend.info();
        if !info.ready {
            return Err(BackendError::NotReady {
                name: info.name,
                reason: info.status,
            });
        }
        backend.issue(request).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::backend::ScopeHint;
    use async_trait::async_trait;

    struct FakeBackend {
        info: BackendInfo,
        secret: String,
    }

    #[async_trait]
    impl Backend for FakeBackend {
        fn info(&self) -> BackendInfo {
            self.info.clone()
        }
        async fn issue(&self, _request: &CredentialRequest) -> Result<IssuedCredential, BackendError> {
            Ok(IssuedCredential {
                username: "smooth".into(),
                secret: self.secret.clone(),
                expires_at: None,
                backend: self.info.name.clone(),
                session_token: None,
            })
        }
    }

    fn req(server: &str) -> CredentialRequest {
        CredentialRequest {
            server_url: server.into(),
            scope_hint: ScopeHint::Unspecified,
            operator_id: "op".into(),
            bead_id: "pearl".into(),
        }
    }

    fn fake(name: &str, globs: &[&str], ready: bool, secret: &str) -> Arc<FakeBackend> {
        Arc::new(FakeBackend {
            info: BackendInfo {
                name: name.into(),
                server_globs: globs.iter().map(|s| (*s).to_string()).collect(),
                ready,
                status: if ready { "ok".into() } else { "logged out".into() },
            },
            secret: secret.into(),
        })
    }

    #[tokio::test]
    async fn routes_to_matching_backend() {
        let registry = BackendRegistry::new()
            .with_backend(fake("gh", &["github.com", "*.github.com"], true, "gho_abc"))
            .with_backend(fake("aws", &["*.amazonaws.com"], true, "AKIA"));
        let cred = registry.issue(&req("https://api.github.com/foo")).await.unwrap();
        assert_eq!(cred.backend, "gh");
        assert_eq!(cred.secret, "gho_abc");

        let cred = registry.issue(&req("sts.amazonaws.com")).await.unwrap();
        assert_eq!(cred.backend, "aws");
    }

    #[tokio::test]
    async fn unknown_server_yields_no_backend() {
        let registry = BackendRegistry::new().with_backend(fake("gh", &["github.com"], true, "x"));
        let err = registry.issue(&req("registry.npmjs.org")).await.unwrap_err();
        assert!(matches!(err, BackendError::NoBackend(_)));
        assert_eq!(err.into_status().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn not_ready_backend_returns_failed_precondition() {
        let registry = BackendRegistry::new().with_backend(fake("gh", &["github.com"], false, "x"));
        let err = registry.issue(&req("github.com")).await.unwrap_err();
        match err {
            BackendError::NotReady { name, reason } => {
                assert_eq!(name, "gh");
                assert_eq!(reason, "logged out");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_server_url_is_invalid() {
        let registry = BackendRegistry::new();
        let err = registry.issue(&req("   ")).await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidServerUrl(_)));
    }

    #[test]
    fn list_backends_preserves_registration_order() {
        let registry = BackendRegistry::new()
            .with_backend(fake("a", &["a.com"], true, "x"))
            .with_backend(fake("b", &["b.com"], false, "y"));
        let infos = registry.list_backends();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].name, "a");
        assert_eq!(infos[1].name, "b");
        assert!(infos[0].ready);
        assert!(!infos[1].ready);
    }

    #[tokio::test]
    async fn first_matching_backend_wins_on_glob_overlap() {
        let registry = BackendRegistry::new()
            .with_backend(fake("specific", &["api.github.com"], true, "specific"))
            .with_backend(fake("wild", &["*.github.com"], true, "wild"));
        let cred = registry.issue(&req("api.github.com")).await.unwrap();
        assert_eq!(cred.backend, "specific");
    }
}
