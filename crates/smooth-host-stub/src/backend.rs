//! Backend trait + domain types for credential issuance.
//!
//! Each `Backend` knows how to mint a credential for one or more
//! server URLs (matched by glob). Concrete backends shell out to the
//! corresponding host CLI; tests provide stub backends with canned
//! responses.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Read vs write hint forwarded by the sandbox.
///
/// Mirrors the proto `ScopeHint`. Backends that don't distinguish
/// ignore this; AWS-STS uses it to pick a role and github-app uses
/// it to choose installation permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeHint {
    Unspecified,
    Read,
    Write,
}

impl From<crate::pb::ScopeHint> for ScopeHint {
    fn from(value: crate::pb::ScopeHint) -> Self {
        match value {
            crate::pb::ScopeHint::Read => Self::Read,
            crate::pb::ScopeHint::Write => Self::Write,
            crate::pb::ScopeHint::Unspecified => Self::Unspecified,
        }
    }
}

/// Input to `Backend::issue`. Carries the audit context (operator +
/// pearl) so backends can log who asked.
#[derive(Debug, Clone)]
pub struct CredentialRequest {
    pub server_url: String,
    pub scope_hint: ScopeHint,
    pub operator_id: String,
    pub bead_id: String,
}

/// A freshly-minted credential. `username` + `secret` follow the
/// Docker credential-helper spec field names (the in-sandbox shims
/// expect those casings).
#[derive(Debug, Clone)]
pub struct IssuedCredential {
    pub username: String,
    pub secret: String,
    /// `None` for long-lived credentials (e.g. a GitHub PAT).
    pub expires_at: Option<DateTime<Utc>>,
    /// Backend identifier (`"gh"`, `"aws-sts"`, etc.) — surfaced in
    /// audit + the TUI status bar.
    pub backend: String,
    /// Optional AWS-STS-style session token. Set by the AWS
    /// backend; the in-sandbox shim places it in
    /// `AWS_SESSION_TOKEN`. Empty / None for backends that don't
    /// issue temporary credentials.
    pub session_token: Option<String>,
}

/// Backend-side errors. Translate to gRPC `Status` codes via
/// `BackendError::into_status`.
#[derive(Debug, Error)]
pub enum BackendError {
    #[error("no backend handles server '{0}'")]
    NoBackend(String),
    #[error("backend '{name}' is not ready: {reason}")]
    NotReady { name: String, reason: String },
    #[error("backend '{name}' failed to mint credential: {source}")]
    Mint {
        name: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("invalid server URL '{0}'")]
    InvalidServerUrl(String),
}

impl BackendError {
    /// Map a backend error onto the gRPC status code that best
    /// describes it. Used by the server adapter.
    #[must_use]
    pub fn into_status(self) -> tonic::Status {
        match self {
            Self::NoBackend(_) => tonic::Status::not_found(self.to_string()),
            Self::NotReady { .. } => tonic::Status::failed_precondition(self.to_string()),
            Self::Mint { .. } => tonic::Status::internal(self.to_string()),
            Self::InvalidServerUrl(_) => tonic::Status::invalid_argument(self.to_string()),
        }
    }
}

/// Backend metadata surfaced via `GetCredentialBackends`. Mirrors
/// the proto `Backend` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub name: String,
    pub server_globs: Vec<String>,
    pub ready: bool,
    pub status: String,
}

/// A credential backend. Implementors handle one logical service
/// (GitHub, AWS, GCR, …). The host stub's `BackendRegistry` routes
/// requests to backends by matching `server_url` against
/// `info().server_globs`.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Backend metadata. `ready=false` causes the registry to skip
    /// this backend during issue and surface the `status` reason
    /// via `NotReady`.
    fn info(&self) -> BackendInfo;

    /// Mint a credential for `request.server_url`. Called only
    /// after the registry has matched the URL against
    /// `info().server_globs`, so the implementation can trust that
    /// the URL falls under one of its globs.
    async fn issue(&self, request: &CredentialRequest) -> Result<IssuedCredential, BackendError>;
}

/// Match `server_url` against a glob pattern. Supports `*.foo.com`
/// (matches `bar.foo.com` and `foo.com`) and exact-string globs
/// (`github.com`). Anything more exotic falls back to literal-string
/// equality.
#[must_use]
pub fn glob_matches(pattern: &str, server_url: &str) -> bool {
    let host = host_of(server_url).unwrap_or(server_url);
    if let Some(stripped) = pattern.strip_prefix("*.") {
        return host == stripped || host.ends_with(&format!(".{stripped}"));
    }
    if pattern.contains('*') {
        // Build a glob::Pattern lazily — full glob semantics.
        return glob::Pattern::new(pattern).map_or(false, |p| p.matches(host));
    }
    pattern == host
}

fn host_of(server_url: &str) -> Option<&str> {
    // Strip scheme + path so backends can match on plain hostnames.
    let no_scheme = server_url.split_once("://").map_or(server_url, |(_, rest)| rest);
    let host = no_scheme.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(host)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_exact_hostname() {
        assert!(glob_matches("github.com", "https://github.com"));
        assert!(glob_matches("github.com", "github.com"));
        assert!(!glob_matches("github.com", "ghe.example.com"));
    }

    #[test]
    fn glob_matches_subdomain_wildcard() {
        assert!(glob_matches("*.amazonaws.com", "https://sts.amazonaws.com/foo"));
        assert!(glob_matches("*.amazonaws.com", "ecr.amazonaws.com"));
        assert!(glob_matches("*.amazonaws.com", "amazonaws.com"));
        assert!(!glob_matches("*.amazonaws.com", "amazonaws.io"));
        assert!(!glob_matches("*.amazonaws.com", "fakeamazonaws.com"));
    }

    #[test]
    fn glob_matches_strips_scheme_and_path() {
        assert!(glob_matches("ghcr.io", "https://ghcr.io/orgs/foo"));
        assert!(glob_matches("ghcr.io", "http://ghcr.io"));
    }

    #[test]
    fn scope_hint_round_trip() {
        for variant in [crate::pb::ScopeHint::Unspecified, crate::pb::ScopeHint::Read, crate::pb::ScopeHint::Write] {
            let domain: ScopeHint = variant.into();
            // Best we can do without a TryFrom back-conversion;
            // just confirm the From impl is exhaustive.
            match domain {
                ScopeHint::Unspecified | ScopeHint::Read | ScopeHint::Write => {}
            }
        }
    }

    #[test]
    fn backend_error_maps_to_grpc_status() {
        assert_eq!(BackendError::NoBackend("x".into()).into_status().code(), tonic::Code::NotFound);
        assert_eq!(
            BackendError::NotReady {
                name: "gh".into(),
                reason: "logged out".into()
            }
            .into_status()
            .code(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(BackendError::InvalidServerUrl("..".into()).into_status().code(), tonic::Code::InvalidArgument);
    }
}
