//! Wire types for the auto-mode access protocol.
//!
//! These shapes are shared by:
//!
//! - **Big Smooth** (`smooth-bigsmooth/src/access.rs::AccessStore`) — files
//!   pending requests, resolves them, broadcasts events.
//! - **The TUI** (`smooth-code/src/auto_mode.rs`) — subscribes to the SSE
//!   stream, parses [`AccessEvent`]s, sends [`AccessResolution`] decisions
//!   back via `/api/access/{approve,deny}`.
//! - **The CLI** (`th access pending|approve|deny`) — same shapes.
//!
//! Keep them in `smooth-narc` so neither the TUI nor the CLI takes a
//! direct dependency on the orchestrator crate. The decision-flow types
//! ([`crate::judge::JudgeRequest`], [`crate::judge::JudgeDecision`])
//! already live here for the same reason.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::judge::Scope;

/// String form of [`crate::judge::JudgeKind`]. The store-side serializes
/// these as plain strings so the protocol doesn't drift if we add a new
/// judge kind without re-stamping every consumer at once.
pub type AccessKind = String;

/// A single pending access request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingAccessRequest {
    pub id: String,
    pub bead_id: String,
    pub operator_id: String,
    /// `network` / `tool` / `file` / `cli` / `mcp` / `port`.
    pub kind: AccessKind,
    /// The resource being requested (domain, command, path…).
    pub resource: String,
    /// Optional extra context for the human: HTTP path, cwd, etc.
    #[serde(default)]
    pub detail: Option<String>,
    /// Narc's reason for asking instead of auto-deciding.
    pub reason: String,
    /// Scopes the human can pick from when resolving.
    pub scope_options: Vec<Scope>,
    pub created_at: DateTime<Utc>,
}

/// Input shape for `AccessStore::file_pending`. Caller fills the fields
/// and the store stamps an id + timestamp.
#[derive(Debug, Clone)]
pub struct NewAccessRequest {
    pub bead_id: String,
    pub operator_id: String,
    pub kind: AccessKind,
    pub resource: String,
    pub detail: Option<String>,
    pub reason: String,
    pub scope_options: Vec<Scope>,
}

impl NewAccessRequest {
    /// Build a request with the default scope ladder. Useful for tests
    /// and callers that always want all four options.
    #[must_use]
    pub fn with_defaults(
        bead_id: impl Into<String>,
        operator_id: impl Into<String>,
        kind: impl Into<String>,
        resource: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            bead_id: bead_id.into(),
            operator_id: operator_id.into(),
            kind: kind.into(),
            resource: resource.into(),
            detail: None,
            reason: reason.into(),
            scope_options: Scope::default_options(),
        }
    }
}

/// A human's resolution of a pending request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessResolution {
    pub id: String,
    pub verdict: ResolutionVerdict,
    pub scope: Scope,
    /// Optional glob the human (or UI) chose to bind the approval to —
    /// e.g. `*.openai.com` instead of just `api.openai.com`. `None`
    /// means "exact resource only". Ignored when denying.
    #[serde(default)]
    pub glob_override: Option<String>,
    pub resolved_at: DateTime<Utc>,
}

/// Approve or deny verdict from the human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionVerdict {
    Approve,
    Deny,
}

impl ResolutionVerdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
        }
    }
}

/// Events broadcast over `/api/access/stream` as the store changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AccessEvent {
    /// A new pending request was filed.
    Pending(PendingAccessRequest),
    /// A previously-pending request was resolved.
    Resolved(AccessResolution),
    /// A previously-pending request expired without resolution.
    Expired { id: String, expired_at: DateTime<Utc> },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn pending_serializes_with_scope_options() {
        let p = PendingAccessRequest {
            id: "abc".into(),
            bead_id: "pearl".into(),
            operator_id: "op".into(),
            kind: "network".into(),
            resource: "api.example.com".into(),
            detail: None,
            reason: "test".into(),
            scope_options: vec![Scope::Once, Scope::Session],
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(json.contains("\"scope_options\":[\"once\",\"session\"]"));
        let back: PendingAccessRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, "abc");
        assert_eq!(back.scope_options.len(), 2);
    }

    #[test]
    fn access_event_uses_tagged_form() {
        let event = AccessEvent::Pending(PendingAccessRequest {
            id: "abc".into(),
            bead_id: "pearl".into(),
            operator_id: "op".into(),
            kind: "network".into(),
            resource: "api.example.com".into(),
            detail: None,
            reason: "test".into(),
            scope_options: vec![Scope::Once],
            created_at: Utc::now(),
        });
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"pending\""));
    }

    #[test]
    fn new_access_request_with_defaults_uses_full_ladder() {
        let req = NewAccessRequest::with_defaults("pearl", "op", "network", "x.example", "reason");
        assert_eq!(req.scope_options.len(), 4);
        assert!(req.detail.is_none());
    }

    #[test]
    fn resolution_verdict_roundtrips() {
        for v in [ResolutionVerdict::Approve, ResolutionVerdict::Deny] {
            let json = serde_json::to_string(&v).unwrap();
            let back: ResolutionVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }
}
