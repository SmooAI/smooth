//! GitHub credential backend — wraps `gh auth token`.
//!
//! Pearl th-893801 Phase 2 iter-4b. The `gh` CLI manages the
//! user's GitHub PAT on the host. We shell out to
//! `gh auth token` to fetch it on demand; readiness is
//! determined by `gh auth status --show-token` succeeding.
//!
//! Why shell out per request instead of caching: the user
//! can `gh auth logout` / log in to a different account
//! between requests. A short shellout is cheap (~50ms) and
//! always reflects current host state.

use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

use crate::backend::{Backend, BackendError, BackendInfo, CredentialRequest, IssuedCredential};

/// Default username used in Docker credential-helper-style
/// responses. The GitHub credential-helper protocol accepts
/// any non-empty username when the secret is a PAT; we use
/// the literal "x-access-token" the way GitHub's own helper
/// does.
const DEFAULT_USERNAME: &str = "x-access-token";

/// Globs the GitHub backend handles by default. Covers
/// github.com itself, every github.com subdomain (api,
/// uploads, raw), the GitHub Container Registry, and the
/// GitHub Package Registry npm host.
fn default_server_globs() -> Vec<String> {
    vec!["github.com".into(), "*.github.com".into(), "ghcr.io".into(), "npm.pkg.github.com".into()]
}

/// Trait for executing a host command. Production uses
/// [`TokioRunner`]; tests inject a stub so they don't depend
/// on a real `gh` binary being installed.
#[async_trait]
pub trait CommandRunner: Send + Sync + 'static {
    /// Run a command with the given args, capturing stdout +
    /// stderr. Returns the exit status, stdout, and stderr
    /// (decoded as utf8-lossy strings).
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<RunOutput>;
}

/// Output of a `CommandRunner::run` call.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Default `CommandRunner` that shells out via `tokio::process::Command`.
pub struct TokioRunner;

#[async_trait]
impl CommandRunner for TokioRunner {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<RunOutput> {
        let output = Command::new(program).args(args).stdin(Stdio::null()).output().await?;
        Ok(RunOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// GitHub backend.
pub struct GitHubBackend {
    runner: Box<dyn CommandRunner>,
    server_globs: Vec<String>,
}

impl GitHubBackend {
    /// Build a GitHub backend using the default
    /// `tokio::process::Command` runner and the default glob
    /// set (`github.com`, `*.github.com`, `ghcr.io`,
    /// `npm.pkg.github.com`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(Box::new(TokioRunner))
    }

    /// Build a backend with a custom runner. Used by tests.
    #[must_use]
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            runner,
            server_globs: default_server_globs(),
        }
    }

    /// Override the default glob set. Useful when the user
    /// has a GHES install on a custom hostname.
    #[must_use]
    pub fn with_globs(mut self, globs: Vec<String>) -> Self {
        self.server_globs = globs;
        self
    }

    async fn auth_status(&self) -> RunOutput {
        // `gh auth status` exits 0 when there's an active
        // login and writes diagnostic info to stderr. Exits
        // non-zero when logged out or `gh` is missing.
        self.runner.run("gh", &["auth", "status"]).await.unwrap_or_else(|e| RunOutput {
            success: false,
            stdout: String::new(),
            stderr: format!("gh auth status failed to spawn: {e}"),
        })
    }
}

impl Default for GitHubBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for GitHubBackend {
    fn info(&self) -> BackendInfo {
        // info() is sync; we can't shell out to check
        // readiness here without a runtime. The registry
        // checks `info().ready` before calling `issue`. To
        // keep info() fast we return `ready=true` and let
        // `issue` surface real auth errors as `BackendError`.
        // The TUI's status pane can call a dedicated
        // readiness probe (follow-up iter) that does shell
        // out.
        BackendInfo {
            name: "gh".into(),
            server_globs: self.server_globs.clone(),
            ready: true,
            status: "host gh CLI; readiness verified per-issue".into(),
        }
    }

    async fn issue(&self, _request: &CredentialRequest) -> Result<IssuedCredential, BackendError> {
        // Confirm there's a logged-in account before pulling
        // a token; this lets us return a clean NotReady
        // instead of an opaque mint failure.
        let status = self.auth_status().await;
        if !status.success {
            return Err(BackendError::NotReady {
                name: "gh".into(),
                reason: format!("gh CLI not logged in: {}", trimmed_stderr(&status.stderr)),
            });
        }

        let output = self.runner.run("gh", &["auth", "token"]).await.map_err(|e| BackendError::Mint {
            name: "gh".into(),
            source: anyhow::anyhow!("failed to spawn `gh auth token`: {e}"),
        })?;
        if !output.success {
            return Err(BackendError::Mint {
                name: "gh".into(),
                source: anyhow::anyhow!("gh auth token failed: {}", trimmed_stderr(&output.stderr)),
            });
        }
        let token = output.stdout.trim();
        if token.is_empty() {
            return Err(BackendError::Mint {
                name: "gh".into(),
                source: anyhow::anyhow!("gh auth token returned an empty string"),
            });
        }
        Ok(IssuedCredential {
            username: DEFAULT_USERNAME.into(),
            secret: token.to_string(),
            // PATs don't carry an explicit expiry in the
            // `gh` output; leave None so callers don't
            // invalidate prematurely.
            expires_at: None,
            backend: "gh".into(),
            session_token: None,
        })
    }
}

fn trimmed_stderr(stderr: &str) -> String {
    stderr.lines().last().unwrap_or("").trim().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::backend::ScopeHint;
    use std::sync::Mutex;

    /// Stub runner returning canned outputs keyed by the
    /// first arg (since `gh auth status` and `gh auth token`
    /// share the program name).
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
            // Match by full args slice.
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
        let backend = GitHubBackend::new();
        let info = backend.info();
        assert_eq!(info.name, "gh");
        assert!(info.server_globs.contains(&"github.com".to_string()));
        assert!(info.server_globs.contains(&"*.github.com".to_string()));
        assert!(info.server_globs.contains(&"ghcr.io".to_string()));
        assert!(info.server_globs.contains(&"npm.pkg.github.com".to_string()));
    }

    #[test]
    fn with_globs_overrides_defaults() {
        let backend = GitHubBackend::new().with_globs(vec!["ghe.example.com".into()]);
        assert_eq!(backend.info().server_globs, vec!["ghe.example.com".to_string()]);
    }

    #[tokio::test]
    async fn issue_returns_token_from_gh_cli() {
        let runner = StubRunner::new(vec![
            (vec!["auth", "status"], ok("Logged in to github.com as smooth-user")),
            (vec!["auth", "token"], ok("gho_abc123\n")),
        ]);
        let backend = GitHubBackend::with_runner(Box::new(runner));
        let cred = backend.issue(&req("https://api.github.com")).await.unwrap();
        assert_eq!(cred.username, DEFAULT_USERNAME);
        assert_eq!(cred.secret, "gho_abc123");
        assert_eq!(cred.backend, "gh");
        assert!(cred.expires_at.is_none());
    }

    #[tokio::test]
    async fn logged_out_user_yields_not_ready() {
        let runner = StubRunner::new(vec![(vec!["auth", "status"], fail("You are not logged into any GitHub hosts."))]);
        let backend = GitHubBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("github.com")).await.unwrap_err();
        match err {
            BackendError::NotReady { name, reason } => {
                assert_eq!(name, "gh");
                assert!(reason.contains("not logged"));
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_token_is_a_mint_error() {
        let runner = StubRunner::new(vec![(vec!["auth", "status"], ok("Logged in")), (vec!["auth", "token"], ok("   \n"))]);
        let backend = GitHubBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("github.com")).await.unwrap_err();
        assert!(matches!(err, BackendError::Mint { .. }));
    }

    #[tokio::test]
    async fn token_failure_propagates_as_mint_error() {
        let runner = StubRunner::new(vec![
            (vec!["auth", "status"], ok("Logged in")),
            (vec!["auth", "token"], fail("token retrieval failed")),
        ]);
        let backend = GitHubBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("github.com")).await.unwrap_err();
        match err {
            BackendError::Mint { name, source } => {
                assert_eq!(name, "gh");
                assert!(source.to_string().contains("token retrieval failed"));
            }
            other => panic!("expected Mint, got {other:?}"),
        }
    }
}
