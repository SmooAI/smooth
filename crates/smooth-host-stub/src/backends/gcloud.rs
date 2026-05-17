//! Gcloud credential backend — wraps `gcloud auth
//! print-access-token`.
//!
//! Pearl th-893801 Phase 2 iter-4d. The host's `gcloud` CLI
//! holds the user's GCP session (via `gcloud auth login` or
//! a service-account key); we trade it for a short-lived
//! OAuth access token per request.
//!
//! Default globs cover Google Container Registry, Artifact
//! Registry, and the common Google API surfaces:
//!
//! * `gcr.io`, `*.gcr.io` — Container Registry
//! * `*.pkg.dev` — Artifact Registry (regional hosts like
//!   `us-central1-docker.pkg.dev`)
//! * `*.googleapis.com` — every Google Cloud API
//!
//! Access tokens are scoped to the user's active gcloud
//! config — Read vs Write doesn't change the shellout (the
//! token's IAM permissions decide). The proto `ScopeHint`
//! is ignored.

use async_trait::async_trait;

use crate::backend::{Backend, BackendError, BackendInfo, CredentialRequest, IssuedCredential};
use crate::backends::github::{CommandRunner, TokioRunner};

/// Default username used in Docker credential-helper-style
/// responses. The Google Container Registry credential
/// helper uses `oauth2accesstoken` as the literal user.
const DEFAULT_USERNAME: &str = "oauth2accesstoken";

fn default_server_globs() -> Vec<String> {
    vec!["gcr.io".into(), "*.gcr.io".into(), "*.pkg.dev".into(), "*.googleapis.com".into()]
}

/// Gcloud backend.
pub struct GcloudBackend {
    runner: Box<dyn CommandRunner>,
    server_globs: Vec<String>,
}

impl GcloudBackend {
    /// Build a backend with the default runner + default globs.
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(Box::new(TokioRunner))
    }

    /// Build a backend with a custom runner. Tests inject a
    /// stub so they don't depend on a real `gcloud` install.
    #[must_use]
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            runner,
            server_globs: default_server_globs(),
        }
    }

    /// Override the default glob set.
    #[must_use]
    pub fn with_globs(mut self, globs: Vec<String>) -> Self {
        self.server_globs = globs;
        self
    }
}

impl Default for GcloudBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for GcloudBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "gcloud".into(),
            server_globs: self.server_globs.clone(),
            ready: true,
            status: "host gcloud CLI; readiness verified per-issue".into(),
        }
    }

    async fn issue(&self, _request: &CredentialRequest) -> Result<IssuedCredential, BackendError> {
        let output = self
            .runner
            .run("gcloud", &["auth", "print-access-token"])
            .await
            .map_err(|e| BackendError::Mint {
                name: "gcloud".into(),
                source: anyhow::anyhow!("failed to spawn gcloud: {e}"),
            })?;
        if !output.success {
            let stderr = trim_stderr(&output.stderr);
            // gcloud surfaces "credentials are not available"
            // when the user is logged out — translate to
            // NotReady so the sandbox sees a clean error.
            if stderr.to_lowercase().contains("credentials") && stderr.to_lowercase().contains("not") {
                return Err(BackendError::NotReady {
                    name: "gcloud".into(),
                    reason: format!("gcloud CLI not logged in: {stderr}"),
                });
            }
            return Err(BackendError::Mint {
                name: "gcloud".into(),
                source: anyhow::anyhow!("gcloud auth print-access-token failed: {stderr}"),
            });
        }
        let token = output.stdout.trim();
        if token.is_empty() {
            return Err(BackendError::Mint {
                name: "gcloud".into(),
                source: anyhow::anyhow!("gcloud auth print-access-token returned an empty string"),
            });
        }
        Ok(IssuedCredential {
            username: DEFAULT_USERNAME.into(),
            secret: token.to_string(),
            // `gcloud auth print-access-token` doesn't print
            // an expiry; tokens last ~1h but the caller
            // shouldn't depend on a specific number.
            expires_at: None,
            backend: "gcloud".into(),
            session_token: None,
        })
    }
}

fn trim_stderr(stderr: &str) -> String {
    stderr.lines().last().unwrap_or("").trim().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::backend::ScopeHint;
    use crate::backends::github::RunOutput;
    use std::sync::Mutex;

    struct StubRunner {
        responses: Mutex<Vec<(Vec<String>, RunOutput)>>,
    }

    impl StubRunner {
        fn new(responses: Vec<(Vec<&str>, RunOutput)>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|(args, out)| (args.into_iter().map(String::from).collect(), out))
                        .collect(),
                ),
            }
        }
    }

    #[async_trait]
    impl CommandRunner for StubRunner {
        async fn run(&self, _program: &str, args: &[&str]) -> std::io::Result<RunOutput> {
            let mut responses = self.responses.lock().unwrap();
            let want: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            let idx = responses
                .iter()
                .position(|(a, _)| *a == want)
                .unwrap_or_else(|| panic!("no stub for args {args:?}"));
            let (_, out) = responses.remove(idx);
            Ok(out)
        }
    }

    fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn fail(stderr: &str) -> RunOutput {
        RunOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
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

    #[test]
    fn info_lists_default_globs() {
        let backend = GcloudBackend::new();
        let info = backend.info();
        assert_eq!(info.name, "gcloud");
        assert!(info.server_globs.contains(&"gcr.io".to_string()));
        assert!(info.server_globs.contains(&"*.gcr.io".to_string()));
        assert!(info.server_globs.contains(&"*.pkg.dev".to_string()));
        assert!(info.server_globs.contains(&"*.googleapis.com".to_string()));
    }

    #[test]
    fn with_globs_overrides_defaults() {
        let backend = GcloudBackend::new().with_globs(vec!["custom.gcp.example".into()]);
        assert_eq!(backend.info().server_globs, vec!["custom.gcp.example".to_string()]);
    }

    #[tokio::test]
    async fn issue_returns_oauth_access_token() {
        let runner = StubRunner::new(vec![(vec!["auth", "print-access-token"], ok("ya29.abc123\n"))]);
        let backend = GcloudBackend::with_runner(Box::new(runner));
        let cred = backend.issue(&req("gcr.io")).await.unwrap();
        assert_eq!(cred.backend, "gcloud");
        assert_eq!(cred.username, DEFAULT_USERNAME);
        assert_eq!(cred.secret, "ya29.abc123");
        assert!(cred.session_token.is_none());
        assert!(cred.expires_at.is_none());
    }

    #[tokio::test]
    async fn logged_out_user_yields_not_ready() {
        let runner = StubRunner::new(vec![(
            vec!["auth", "print-access-token"],
            fail("ERROR: (gcloud.auth.print-access-token) Your current active account does not have any valid credentials"),
        )]);
        let backend = GcloudBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("gcr.io")).await.unwrap_err();
        match err {
            BackendError::NotReady { name, reason } => {
                assert_eq!(name, "gcloud");
                assert!(reason.to_lowercase().contains("not logged"));
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_token_is_a_mint_error() {
        let runner = StubRunner::new(vec![(vec!["auth", "print-access-token"], ok("   \n"))]);
        let backend = GcloudBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("gcr.io")).await.unwrap_err();
        assert!(matches!(err, BackendError::Mint { .. }));
    }

    #[tokio::test]
    async fn generic_failure_propagates_as_mint_error() {
        let runner = StubRunner::new(vec![(
            vec!["auth", "print-access-token"],
            fail("ERROR: (gcloud.auth.print-access-token) something else broke"),
        )]);
        let backend = GcloudBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("gcr.io")).await.unwrap_err();
        match err {
            BackendError::Mint { name, source } => {
                assert_eq!(name, "gcloud");
                assert!(source.to_string().contains("something else broke"));
            }
            other => panic!("expected Mint, got {other:?}"),
        }
    }
}
