//! Credential broker — mints short-lived credentials for the sandbox
//! after a human approves the issue request.
//!
//! Pearl th-08b65f. The sandbox never holds long-lived credentials.
//! When a tool inside the VM needs to authenticate (git clone over
//! https, gh CLI, AWS API), it asks `smooth-credential-helper`, which
//! POSTs to Big Smooth's `/api/creds/issue`. Big Smooth either:
//!
//! 1. Finds the request server matches a pre-approved scope in
//!    `wonk-allow.toml` (e.g. user previously approved `github.com`
//!    at scope `user`) and mints immediately, OR
//! 2. Files an `Ask` into the AccessStore — same Decision flow as
//!    every other tool gate, surfaces as a TUI card — and waits for
//!    the human. On approve, mints. On deny / timeout, 403.
//!
//! The "mint" step calls a per-server backend. v1 supports
//! `github.com` via the host's `gh auth token` (so the agent inherits
//! the user's logged-in GitHub session for the duration of the
//! credential's TTL). Other backends (AWS STS, Docker Hub, generic
//! username/password) are follow-up pearls.
//!
//! ## Security
//!
//! - The minted credential is short-lived: the broker doesn't extend
//!   a long-lived PAT; for GitHub it forwards the user's `gh` session
//!   token which is rotated by gh on its own schedule. Future work
//!   (file separately): mint a GitHub App installation token per
//!   pearl with a 1-hour TTL and exfiltrate-by-design.
//! - The credential leaves the broker over the same loopback /
//!   already-approved channel the sandbox uses for `host_tool`. It
//!   doesn't get persisted to disk inside the VM.
//! - The broker logs every issue at INFO so the audit pipeline picks
//!   up which sandbox + which pearl asked for which server.

use serde::{Deserialize, Serialize};

/// A credential issued to a sandbox. Wire shape matches the Docker
/// credential-helper spec: `Username` + `Secret` (both capitalized).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Credential {
    /// Username for the server. For OAuth / token-only flows this is
    /// the literal "oauth2" or "x-access-token" (git understands the
    /// HTTP basic auth shape this implies).
    pub username: String,
    /// The bearer token / secret. Treat as sensitive — never log.
    pub secret: String,
}

/// What the credential is needed for. The broker maps this onto a
/// backend (gh, AWS STS, …). Defaults to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScopeHint {
    /// Read-only (clone, fetch, list).
    #[default]
    Read,
    /// Read + write (push, commit, upload).
    Write,
}

/// Errors from the broker. Surfaced both at the Rust API layer and
/// through `/api/creds/issue` (mapped to HTTP status codes).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredsError {
    /// The server hostname isn't one we have a mint backend for.
    UnsupportedServer { server: String },
    /// Human denied the request (or timeout fired).
    Denied { reason: String },
    /// The mint backend errored. For gh that usually means the user
    /// isn't logged in (`gh auth login` solves it).
    MintFailed { message: String },
}

impl std::fmt::Display for CredsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedServer { server } => write!(f, "no credential mint backend for server '{server}'"),
            Self::Denied { reason } => write!(f, "credential request denied: {reason}"),
            Self::MintFailed { message } => write!(f, "mint backend failed: {message}"),
        }
    }
}

impl std::error::Error for CredsError {}

/// Pick the credential-mint backend that handles a given `ServerURL`.
/// Returns `None` if no backend matches — caller surfaces this as
/// [`CredsError::UnsupportedServer`].
///
/// Recognized servers (v1):
///   - `github.com` / `https://github.com` / `https://api.github.com`
///     → mints via `gh auth token`
///   - `https://x.com` / `https://twitter.com` → reserved for future
///     work (Bluesky / X integration); returns `None` for now
///
/// AWS / Docker Hub / npmjs are deliberately left out of v1 — each
/// needs a meaningfully different broker shape.
#[must_use]
pub fn pick_backend(server_url: &str) -> Option<MintBackend> {
    let host = extract_host(server_url)?;
    let lower = host.to_ascii_lowercase();
    if lower == "github.com" || lower == "api.github.com" || lower.ends_with(".github.com") {
        Some(MintBackend::Github)
    } else {
        None
    }
}

/// What backend the broker dispatches to for a given server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintBackend {
    Github,
}

/// Mint a credential for `server` via the chosen backend.
///
/// # Errors
///
/// Returns [`CredsError::UnsupportedServer`] if `pick_backend`
/// doesn't match. Returns [`CredsError::MintFailed`] when the
/// backend binary errors (e.g. `gh auth login` hasn't been run).
pub async fn mint(server_url: &str, _scope: ScopeHint) -> Result<Credential, CredsError> {
    let Some(backend) = pick_backend(server_url) else {
        return Err(CredsError::UnsupportedServer { server: server_url.into() });
    };
    match backend {
        MintBackend::Github => mint_github().await,
    }
}

/// Call `gh auth token` and wrap the resulting bearer in a
/// Credential shaped for Docker's spec. Username is `x-access-token`
/// — git's https credential helper accepts that as a sentinel for
/// "the secret IS the bearer", which is the shape `gh` itself uses
/// for cloning private repos.
async fn mint_github() -> Result<Credential, CredsError> {
    // Resolve the gh binary the same way host_tool does — launchd's
    // minimal PATH doesn't include /opt/homebrew/bin where gh
    // usually lives, so we walk a richer search list.
    let gh_path = resolve_binary("gh").ok_or(CredsError::MintFailed {
        message: "gh binary not found in PATH; install GitHub CLI and `gh auth login`".into(),
    })?;
    let out = tokio::process::Command::new(&gh_path)
        .args(["auth", "token"])
        .output()
        .await
        .map_err(|e| CredsError::MintFailed {
            message: format!("spawn gh: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CredsError::MintFailed {
            message: if stderr.is_empty() {
                format!("gh auth token exited {}", out.status)
            } else {
                stderr
            },
        });
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if token.is_empty() {
        return Err(CredsError::MintFailed {
            message: "gh auth token returned empty output — run `gh auth login`".into(),
        });
    }
    Ok(Credential {
        // git's https credential helper treats `x-access-token` as
        // "the secret is the PAT/installation token"; works for both
        // PAT and GitHub App tokens.
        username: "x-access-token".into(),
        secret: token,
    })
}

/// Resolve a binary against the same richer-than-launchd search list
/// `host_tools::resolve_tool_path` uses. Public so the helper binary
/// (or anyone else who needs the same resolution) can share it.
#[must_use]
pub fn resolve_binary(name: &str) -> Option<String> {
    if name.contains('/') {
        return None;
    }
    const SEARCH: &[&str] = &["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin", "/sbin", "/usr/sbin"];
    for dir in SEARCH {
        let p = std::path::Path::new(dir).join(name);
        if p.is_file() {
            return p.to_str().map(String::from);
        }
    }
    None
}

/// Extract the host part of a `https://host[:port]/path` URL. Bare
/// hostnames (no scheme) are returned as-is. Returns `None` for
/// inputs that don't contain a recognizable host.
fn extract_host(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip the scheme if present.
    let without_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    // Strip userinfo (rare but possible).
    let after_userinfo = without_scheme.rsplit_once('@').map_or(without_scheme, |(_, rest)| rest);
    // Cut at the first `/`, `?`, or `#`.
    let host_with_port = after_userinfo.split(['/', '?', '#']).next()?;
    if host_with_port.is_empty() {
        return None;
    }
    // Strip the port — for IPv6 this would need bracket handling,
    // but `:port` on a bracketless v4/hostname is fine.
    let host = host_with_port.rsplit_once(':').map_or(host_with_port, |(h, _)| h);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn pick_backend_matches_github_shapes() {
        assert_eq!(pick_backend("https://github.com").unwrap(), MintBackend::Github);
        assert_eq!(pick_backend("https://github.com/foo/bar").unwrap(), MintBackend::Github);
        assert_eq!(pick_backend("github.com").unwrap(), MintBackend::Github);
        assert_eq!(pick_backend("https://api.github.com/repos/foo").unwrap(), MintBackend::Github);
        assert_eq!(pick_backend("https://codeload.github.com/x").unwrap(), MintBackend::Github);
    }

    #[test]
    fn pick_backend_rejects_unknown_servers() {
        assert!(pick_backend("https://gitlab.com").is_none());
        assert!(pick_backend("https://bitbucket.org/foo").is_none());
        assert!(pick_backend("https://docker.io/library/alpine").is_none());
        assert!(pick_backend("").is_none());
    }

    #[test]
    fn extract_host_handles_common_shapes() {
        assert_eq!(extract_host("https://github.com/foo/bar"), Some("github.com"));
        assert_eq!(extract_host("github.com"), Some("github.com"));
        assert_eq!(extract_host("https://github.com:443/foo"), Some("github.com"));
        assert_eq!(extract_host("https://user:pass@example.com/path"), Some("example.com"));
        assert_eq!(extract_host(""), None);
        assert_eq!(extract_host("not-a-url"), Some("not-a-url"));
    }

    #[test]
    fn credential_serializes_as_pascal_case() {
        // Docker credential-helper spec: `Username` + `Secret`
        // (capitalized). git's credential helper consumers depend on
        // this exact casing.
        let c = Credential {
            username: "x-access-token".into(),
            secret: "ghs_aaaaaa".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"Username\""), "must use Username: {json}");
        assert!(json.contains("\"Secret\""), "must use Secret: {json}");
        // Round-trips.
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.username, "x-access-token");
        assert_eq!(back.secret, "ghs_aaaaaa");
    }

    #[test]
    fn scope_hint_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&ScopeHint::Read).unwrap(), "\"read\"");
        assert_eq!(serde_json::to_string(&ScopeHint::Write).unwrap(), "\"write\"");
        let r: ScopeHint = serde_json::from_str("\"read\"").unwrap();
        assert_eq!(r, ScopeHint::Read);
    }

    #[test]
    fn scope_hint_defaults_to_read() {
        assert_eq!(ScopeHint::default(), ScopeHint::Read);
    }

    #[test]
    fn creds_error_display_carries_context() {
        let e = CredsError::UnsupportedServer {
            server: "https://gitlab.com".into(),
        };
        assert!(e.to_string().contains("gitlab.com"));
        let e = CredsError::MintFailed {
            message: "gh not logged in".into(),
        };
        assert!(e.to_string().contains("gh not logged in"));
        let e = CredsError::Denied {
            reason: "human said no".into(),
        };
        assert!(e.to_string().contains("human said no"));
    }

    #[tokio::test]
    async fn mint_unknown_server_errors_unsupported() {
        let err = mint("https://example.com", ScopeHint::Read).await.expect_err("unsupported");
        assert!(matches!(err, CredsError::UnsupportedServer { .. }));
    }

    #[test]
    fn resolve_binary_rejects_path_separators() {
        // Defense in depth: even if a caller tried to slip
        // `../../etc/passwd` past the protocol layer, the helper's
        // binary resolver won't return it.
        assert!(resolve_binary("/etc/passwd").is_none());
        assert!(resolve_binary("../etc/passwd").is_none());
    }
}
