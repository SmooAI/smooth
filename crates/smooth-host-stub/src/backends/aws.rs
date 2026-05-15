//! AWS-STS credential backend — wraps `aws sts get-session-token`
//! and `aws sts assume-role`.
//!
//! Pearl th-893801 Phase 2 iter-4c. The `aws` CLI on the host has
//! the user's long-term access keys configured (via
//! `aws configure` or AWS SSO); we trade them for short-lived
//! session credentials per request.
//!
//! Scope-hint mapping:
//!
//! * `Read` / `Unspecified` → `aws sts get-session-token` —
//!   yields temp creds tied to the calling identity.
//! * `Write` → `aws sts assume-role
//!   --role-arn $SMOOTH_AWS_WRITE_ROLE_ARN --role-session-name
//!   smooth-<operator>` — when the env var is unset, falls back
//!   to `get-session-token` and logs a warning.
//!
//! The CLI returns JSON shaped like:
//!
//! ```text
//! {
//!   "Credentials": {
//!     "AccessKeyId": "...",
//!     "SecretAccessKey": "...",
//!     "SessionToken": "...",
//!     "Expiration": "2026-05-15T18:00:00+00:00"
//!   }
//! }
//! ```
//!
//! The `session_token` proto field (added in iter-4c) carries
//! `SessionToken` end-to-end; in-sandbox shims place it in
//! `AWS_SESSION_TOKEN` so every AWS SDK / `aws` CLI call inside
//! the VM picks it up automatically.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::backend::{Backend, BackendError, BackendInfo, CredentialRequest, IssuedCredential, ScopeHint};
use crate::backends::github::{CommandRunner, RunOutput, TokioRunner};

/// Env var the user sets to opt into Write-scope role
/// assumption. Empty / unset means Write falls back to
/// `get-session-token`.
pub const WRITE_ROLE_ARN_ENV: &str = "SMOOTH_AWS_WRITE_ROLE_ARN";

fn default_server_globs() -> Vec<String> {
    vec!["*.amazonaws.com".into(), "*.aws.amazon.com".into()]
}

/// Build a `BackendInfo` for this backend.
fn backend_info(globs: &[String]) -> BackendInfo {
    BackendInfo {
        name: "aws-sts".into(),
        server_globs: globs.to_vec(),
        ready: true,
        status: "host aws CLI; readiness verified per-issue".into(),
    }
}

/// Returns the assume-role ARN from the env, if set + non-empty.
fn write_role_arn() -> Option<String> {
    std::env::var(WRITE_ROLE_ARN_ENV).ok().filter(|v| !v.trim().is_empty())
}

/// AWS-STS backend.
pub struct AwsStsBackend {
    runner: Box<dyn CommandRunner>,
    server_globs: Vec<String>,
    /// Role ARN used when scope_hint=Write. Defaults to the
    /// `SMOOTH_AWS_WRITE_ROLE_ARN` env var if non-empty; None
    /// falls back to `get-session-token` for Write requests
    /// (and logs a warning). Tests construct the backend with
    /// an explicit value to avoid env-var racing across
    /// parallel test runs.
    write_role_arn: Option<String>,
}

impl AwsStsBackend {
    /// Build a backend with the default runner + default globs.
    /// Honors `SMOOTH_AWS_WRITE_ROLE_ARN` for the write role.
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(Box::new(TokioRunner))
    }

    /// Build a backend with a custom runner. Tests inject a
    /// stub so they don't depend on a real `aws` install.
    /// Honors `SMOOTH_AWS_WRITE_ROLE_ARN` for the write role.
    #[must_use]
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            runner,
            server_globs: default_server_globs(),
            write_role_arn: write_role_arn(),
        }
    }

    /// Override the default glob set.
    #[must_use]
    pub fn with_globs(mut self, globs: Vec<String>) -> Self {
        self.server_globs = globs;
        self
    }

    /// Explicit write role ARN, bypassing the env var. Used by
    /// tests to avoid env-var racing across parallel test
    /// invocations.
    #[must_use]
    pub fn with_write_role_arn(mut self, role_arn: Option<String>) -> Self {
        self.write_role_arn = role_arn;
        self
    }

    async fn run_aws(&self, args: &[&str]) -> Result<RunOutput, BackendError> {
        self.runner.run("aws", args).await.map_err(|e| BackendError::Mint {
            name: "aws-sts".into(),
            source: anyhow::anyhow!("failed to spawn aws: {e}"),
        })
    }
}

impl Default for AwsStsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct StsResponse {
    #[serde(rename = "Credentials")]
    credentials: StsCredentials,
}

#[derive(Debug, Deserialize)]
struct StsCredentials {
    #[serde(rename = "AccessKeyId")]
    access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: String,
    #[serde(rename = "SessionToken")]
    session_token: String,
    #[serde(rename = "Expiration")]
    expiration: Option<String>,
}

fn parse_sts_json(stdout: &str) -> Result<StsCredentials, BackendError> {
    let resp: StsResponse = serde_json::from_str(stdout).map_err(|e| BackendError::Mint {
        name: "aws-sts".into(),
        source: anyhow::anyhow!("failed to parse sts JSON: {e}; stdout was {stdout:?}"),
    })?;
    let creds = resp.credentials;
    if creds.access_key_id.is_empty() || creds.secret_access_key.is_empty() || creds.session_token.is_empty() {
        return Err(BackendError::Mint {
            name: "aws-sts".into(),
            source: anyhow::anyhow!("sts response missing required credential fields"),
        });
    }
    Ok(creds)
}

fn parse_expiration(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

#[async_trait]
impl Backend for AwsStsBackend {
    fn info(&self) -> BackendInfo {
        backend_info(&self.server_globs)
    }

    async fn issue(&self, request: &CredentialRequest) -> Result<IssuedCredential, BackendError> {
        // Pick the sts subcommand from the scope hint. Write
        // requires SMOOTH_AWS_WRITE_ROLE_ARN; if it's missing
        // we degrade to get-session-token (Read semantics) and
        // log so the user notices.
        let session_name = format!("smooth-{}", if request.operator_id.is_empty() { "op" } else { request.operator_id.as_str() });
        let role_arn_for_write = match request.scope_hint {
            ScopeHint::Write => self.write_role_arn.clone(),
            _ => None,
        };

        let output = if let Some(role_arn) = role_arn_for_write.as_deref() {
            self.run_aws(&[
                "sts",
                "assume-role",
                "--role-arn",
                role_arn,
                "--role-session-name",
                session_name.as_str(),
                "--output",
                "json",
            ])
            .await?
        } else {
            if matches!(request.scope_hint, ScopeHint::Write) {
                tracing::warn!(
                    operator = %request.operator_id,
                    "AWS Write scope requested but {WRITE_ROLE_ARN_ENV} not set — falling back to get-session-token"
                );
            }
            self.run_aws(&["sts", "get-session-token", "--output", "json"]).await?
        };
        if !output.success {
            // STS errors are descriptive; pass the last line
            // back so the sandbox can surface it.
            return Err(BackendError::Mint {
                name: "aws-sts".into(),
                source: anyhow::anyhow!("aws sts failed: {}", trim_stderr(&output.stderr)),
            });
        }
        let creds = parse_sts_json(&output.stdout)?;
        Ok(IssuedCredential {
            username: creds.access_key_id,
            secret: creds.secret_access_key,
            expires_at: parse_expiration(creds.expiration),
            backend: "aws-sts".into(),
            session_token: Some(creds.session_token),
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

    fn req(server: &str, scope: ScopeHint) -> CredentialRequest {
        CredentialRequest {
            server_url: server.into(),
            scope_hint: scope,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
        }
    }

    const STS_JSON: &str = r#"{
        "Credentials": {
            "AccessKeyId": "ASIA1234",
            "SecretAccessKey": "verysecret",
            "SessionToken": "FwoGZ...token...",
            "Expiration": "2026-05-15T18:00:00+00:00"
        }
    }"#;

    #[test]
    fn info_lists_default_globs() {
        let backend = AwsStsBackend::new();
        let info = backend.info();
        assert_eq!(info.name, "aws-sts");
        assert!(info.server_globs.contains(&"*.amazonaws.com".to_string()));
        assert!(info.server_globs.contains(&"*.aws.amazon.com".to_string()));
    }

    #[tokio::test]
    async fn read_scope_runs_get_session_token() {
        let runner = StubRunner::new(vec![(vec!["sts", "get-session-token", "--output", "json"], ok(STS_JSON))]);
        let backend = AwsStsBackend::with_runner(Box::new(runner));
        let cred = backend.issue(&req("sts.amazonaws.com", ScopeHint::Read)).await.unwrap();
        assert_eq!(cred.backend, "aws-sts");
        assert_eq!(cred.username, "ASIA1234");
        assert_eq!(cred.secret, "verysecret");
        assert_eq!(cred.session_token.as_deref(), Some("FwoGZ...token..."));
        assert!(cred.expires_at.is_some());
    }

    #[tokio::test]
    async fn unspecified_scope_also_gets_session_token() {
        let runner = StubRunner::new(vec![(vec!["sts", "get-session-token", "--output", "json"], ok(STS_JSON))]);
        let backend = AwsStsBackend::with_runner(Box::new(runner));
        let cred = backend.issue(&req("ecr.amazonaws.com", ScopeHint::Unspecified)).await.unwrap();
        assert_eq!(cred.backend, "aws-sts");
    }

    #[tokio::test]
    async fn write_scope_with_role_arn_assumes_role() {
        let runner = StubRunner::new(vec![(
            vec![
                "sts",
                "assume-role",
                "--role-arn",
                "arn:aws:iam::1234:role/SmoothWrite",
                "--role-session-name",
                "smooth-op-1",
                "--output",
                "json",
            ],
            ok(STS_JSON),
        )]);
        let backend = AwsStsBackend::with_runner(Box::new(runner)).with_write_role_arn(Some("arn:aws:iam::1234:role/SmoothWrite".into()));
        let cred = backend.issue(&req("sts.amazonaws.com", ScopeHint::Write)).await.unwrap();
        assert_eq!(cred.backend, "aws-sts");
        assert_eq!(cred.username, "ASIA1234");
    }

    #[tokio::test]
    async fn write_scope_without_role_arn_falls_back_to_session_token() {
        let runner = StubRunner::new(vec![(vec!["sts", "get-session-token", "--output", "json"], ok(STS_JSON))]);
        let backend = AwsStsBackend::with_runner(Box::new(runner)).with_write_role_arn(None);
        let cred = backend.issue(&req("sts.amazonaws.com", ScopeHint::Write)).await.unwrap();
        assert_eq!(cred.backend, "aws-sts");
        assert_eq!(cred.session_token.as_deref(), Some("FwoGZ...token..."));
    }

    #[tokio::test]
    async fn sts_failure_propagates_as_mint_error() {
        let runner = StubRunner::new(vec![(
            vec!["sts", "get-session-token", "--output", "json"],
            fail("An error occurred (ExpiredToken) when calling the GetSessionToken operation"),
        )]);
        let backend = AwsStsBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("sts.amazonaws.com", ScopeHint::Read)).await.unwrap_err();
        match err {
            BackendError::Mint { name, source } => {
                assert_eq!(name, "aws-sts");
                assert!(source.to_string().contains("ExpiredToken"));
            }
            other => panic!("expected Mint, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_sts_json_is_a_mint_error() {
        let runner = StubRunner::new(vec![(vec!["sts", "get-session-token", "--output", "json"], ok("not valid json"))]);
        let backend = AwsStsBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("sts.amazonaws.com", ScopeHint::Read)).await.unwrap_err();
        assert!(matches!(err, BackendError::Mint { .. }));
    }

    #[tokio::test]
    async fn sts_response_missing_session_token_is_an_error() {
        let bad = r#"{"Credentials":{"AccessKeyId":"x","SecretAccessKey":"y","SessionToken":"","Expiration":null}}"#;
        let runner = StubRunner::new(vec![(vec!["sts", "get-session-token", "--output", "json"], ok(bad))]);
        let backend = AwsStsBackend::with_runner(Box::new(runner));
        let err = backend.issue(&req("sts.amazonaws.com", ScopeHint::Read)).await.unwrap_err();
        assert!(matches!(err, BackendError::Mint { .. }));
    }

    #[test]
    fn parse_expiration_handles_rfc3339() {
        let dt = parse_expiration(Some("2026-05-15T18:00:00+00:00".into())).unwrap();
        assert_eq!(dt.timestamp(), DateTime::parse_from_rfc3339("2026-05-15T18:00:00Z").unwrap().timestamp());
    }

    #[test]
    fn parse_expiration_returns_none_for_garbage() {
        assert!(parse_expiration(Some("not a date".into())).is_none());
        assert!(parse_expiration(None).is_none());
    }
}
