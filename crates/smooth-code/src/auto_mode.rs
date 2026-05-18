//! TUI side of the Claude-Code-style auto-mode permission UX.
//!
//! Big Smooth holds a tool call open whenever Safehouse Narc returns
//! [`smooth_narc::Decision::Ask`] (pearl th-49b4aa). The TUI hooks in
//! through two pieces:
//!
//! - **SSE subscriber** (`spawn_subscriber`) — long-running tokio task
//!   that connects to `/api/access/stream`, parses each
//!   [`smooth_narc::AccessEvent`], and updates the in-memory list of
//!   prompts that the renderer draws inline in the chat.
//! - **HTTP client** (`resolve`) — POSTs to `/api/access/{approve,deny}`
//!   when the user picks a key on a prompt card.
//!
//! Prompts live on `AppState::permission_prompts`. Each
//! [`PermissionPromptState`] starts in [`PromptStatus::Open`]; key handlers
//! flip it to [`PromptStatus::Resolving`] while the POST is in flight,
//! then to [`PromptStatus::Approved`] / [`PromptStatus::Denied`] /
//! [`PromptStatus::Expired`] when the SSE stream confirms or a deadline
//! passes. The card renderer reads the status to pick its label + glyph.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::Serialize;
use smooth_narc::judge::Scope;
use smooth_narc::{AccessEvent, PendingAccessRequest, ResolutionVerdict};

use crate::state::AppState;

/// Display status of an inline permission prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptStatus {
    /// Filed and awaiting a human keystroke.
    Open,
    /// User hit a key; the POST is in flight.
    Resolving { verdict: ResolutionVerdict, scope: Scope },
    /// Resolved as Approve at the named scope.
    Approved { scope: Scope, glob: Option<String> },
    /// Resolved as Deny at the named scope.
    Denied { scope: Scope },
    /// The server expired the request before any key was pressed.
    Expired,
    /// Something went wrong talking to the server. The user can retry.
    Failed { reason: String },
}

impl PromptStatus {
    /// True when the prompt is still interactive (key handlers act on it).
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    /// Short status glyph + label rendered after a resolved prompt
    /// collapses. The label is `&'static str` so the renderer can drop
    /// it into a [`ratatui::text::Span`] without allocation.
    #[must_use]
    pub fn collapsed_label(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Open => None,
            Self::Resolving { .. } => Some(("⋯", "resolving")),
            Self::Approved { .. } => Some(("✓", "approved")),
            Self::Denied { .. } => Some(("✗", "denied")),
            Self::Expired => Some(("◌", "expired")),
            Self::Failed { .. } => Some(("!", "failed")),
        }
    }
}

/// A single permission prompt as seen by the TUI.
#[derive(Debug, Clone)]
pub struct PermissionPromptState {
    pub request: PendingAccessRequest,
    pub status: PromptStatus,
    /// Wall-clock when the prompt was first filed in the local model.
    /// Used purely for stable rendering order; the authoritative
    /// timestamp is `request.created_at`.
    pub seen_at: DateTime<Utc>,
}

impl PermissionPromptState {
    #[must_use]
    pub fn new(request: PendingAccessRequest) -> Self {
        Self {
            request,
            status: PromptStatus::Open,
            seen_at: Utc::now(),
        }
    }
}

/// Apply an [`AccessEvent`] to the live prompt list. Splitting this out
/// of the SSE subscriber lets us drive the same state machine from
/// tests without standing up a server.
pub fn apply_event(prompts: &mut Vec<PermissionPromptState>, event: AccessEvent) {
    match event {
        AccessEvent::Pending(req) => {
            // Skip duplicates — the SSE stream can re-deliver if a slow
            // reader misses an event and re-syncs via /api/access/pending.
            if prompts.iter().any(|p| p.request.id == req.id) {
                return;
            }
            prompts.push(PermissionPromptState::new(req));
        }
        AccessEvent::Resolved(res) => {
            if let Some(p) = prompts.iter_mut().find(|p| p.request.id == res.id) {
                p.status = match res.verdict {
                    ResolutionVerdict::Approve => PromptStatus::Approved {
                        scope: res.scope,
                        glob: res.glob_override,
                    },
                    ResolutionVerdict::Deny => PromptStatus::Denied { scope: res.scope },
                };
            }
        }
        AccessEvent::Expired { id, .. } => {
            if let Some(p) = prompts.iter_mut().find(|p| p.request.id == id) {
                p.status = PromptStatus::Expired;
            }
        }
    }
}

/// Body shape POSTed to `/api/access/{approve,deny}`.
#[derive(Debug, Clone, Serialize)]
struct ResolveBody<'a> {
    id: &'a str,
    scope: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    glob_override: Option<&'a str>,
}

/// Send a resolution to Big Smooth. Used by the key handlers in app.rs.
/// Returns the HTTP status text on failure so the TUI can render
/// `[Failed: HTTP 404]` next to the prompt.
///
/// # Errors
///
/// Network errors, non-2xx status codes, and connection-refused are all
/// surfaced as the `Err` arm; callers should flip the prompt status to
/// [`PromptStatus::Failed`] with the returned string.
pub async fn resolve(
    base_url: &str,
    client: &reqwest::Client,
    id: &str,
    verdict: ResolutionVerdict,
    scope: Scope,
    glob_override: Option<&str>,
) -> Result<(), String> {
    let path = match verdict {
        ResolutionVerdict::Approve => "approve",
        ResolutionVerdict::Deny => "deny",
    };
    let url = format!("{base_url}/api/access/{path}");
    let body = ResolveBody {
        id,
        scope: scope.as_str(),
        // Don't send glob_override on denies — it'd be ignored anyway.
        glob_override: if matches!(verdict, ResolutionVerdict::Approve) { glob_override } else { None },
    };
    let resp = client.post(&url).json(&body).send().await.map_err(|e| format!("network error: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(format!("HTTP {status}: {text}"))
    }
}

/// Connect to `/api/access/stream` and apply each incoming event to the
/// shared state. Reconnects with exponential backoff on disconnect so
/// the TUI survives a Big Smooth restart.
///
/// Returns when `state` is dropped (the strong-count check happens on
/// each iteration). Spawn it once at TUI startup with
/// [`spawn_subscriber`].
pub async fn run_subscriber(base_url: String, state: Arc<Mutex<AppState>>) {
    let url = format!("{base_url}/api/access/stream");
    // Default reqwest client has no top-level timeout — important for
    // SSE streams which intentionally hold the connection open.
    // Setting `.timeout(Duration::ZERO)` actually arms a 0-second
    // deadline, so the right move is to NOT call .timeout() at all.
    let client = reqwest::Client::new();
    let mut backoff = Duration::from_millis(500);
    let max_backoff = Duration::from_secs(30);

    loop {
        // Stop if the only Arc left is this task's own (i.e. AppState
        // was dropped). One strong count means just us; nothing else
        // is watching the prompts.
        if Arc::strong_count(&state) <= 1 {
            tracing::debug!("auto_mode subscriber: state dropped, exiting");
            return;
        }

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(url = %url, "auto_mode subscriber: connected");
                // Reset backoff after a healthy connect.
                backoff = Duration::from_millis(500);
                let mut byte_stream = resp.bytes_stream();
                // SSE delivers `data: <json>\n\n` chunks. Parse with a
                // tiny line buffer so we don't pull in the eventsource
                // crate just for this.
                let mut buffer = String::new();
                while let Some(chunk) = byte_stream.next().await {
                    let Ok(bytes) = chunk else {
                        break;
                    };
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    tracing::trace!(buffer_len = buffer.len(), "auto_mode subscriber: chunk received");
                    while let Some(end) = buffer.find("\n\n") {
                        let raw_event: String = buffer.drain(..end + 2).collect();
                        for line in raw_event.lines() {
                            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                                continue;
                            };
                            if data.is_empty() {
                                continue;
                            }
                            let Ok(event) = serde_json::from_str::<AccessEvent>(data) else {
                                tracing::warn!(data, "auto_mode subscriber: failed to parse event");
                                continue;
                            };
                            if let Ok(mut s) = state.lock() {
                                apply_event(&mut s.permission_prompts, event);
                            }
                        }
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "auto_mode subscriber: non-success status, will retry");
            }
            Err(e) => {
                tracing::debug!(error = %e, "auto_mode subscriber: connect error, will retry");
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

/// Spawn the SSE subscriber as a detached tokio task. The handle is
/// dropped because the loop terminates on `Arc::strong_count == 1`.
pub fn spawn_subscriber(base_url: String, state: Arc<Mutex<AppState>>) {
    tokio::spawn(run_subscriber(base_url, state));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn pending(id: &str) -> PendingAccessRequest {
        PendingAccessRequest {
            id: id.into(),
            bead_id: "pearl".into(),
            operator_id: "op".into(),
            kind: "network".into(),
            resource: "api.example.com".into(),
            detail: Some("GET /v1/models".into()),
            reason: "domain not in allowlist".into(),
            scope_options: Scope::default_options(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn pending_event_appends_prompt() {
        let mut prompts = Vec::new();
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].request.id, "abc");
        assert!(prompts[0].status.is_open());
    }

    #[test]
    fn duplicate_pending_event_is_idempotent() {
        // SSE can re-deliver if the client falls behind and re-syncs.
        // Re-applying the same Pending event must not duplicate.
        let mut prompts = Vec::new();
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        assert_eq!(prompts.len(), 1);
    }

    #[test]
    fn resolved_event_flips_status_to_approved() {
        let mut prompts = Vec::new();
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        apply_event(
            &mut prompts,
            AccessEvent::Resolved(smooth_narc::AccessResolution {
                id: "abc".into(),
                verdict: ResolutionVerdict::Approve,
                scope: Scope::Session,
                glob_override: Some("*.example.com".into()),
                resolved_at: Utc::now(),
            }),
        );
        match &prompts[0].status {
            PromptStatus::Approved { scope, glob } => {
                assert_eq!(*scope, Scope::Session);
                assert_eq!(glob.as_deref(), Some("*.example.com"));
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn resolved_event_flips_status_to_denied() {
        let mut prompts = Vec::new();
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        apply_event(
            &mut prompts,
            AccessEvent::Resolved(smooth_narc::AccessResolution {
                id: "abc".into(),
                verdict: ResolutionVerdict::Deny,
                scope: Scope::Once,
                glob_override: None,
                resolved_at: Utc::now(),
            }),
        );
        assert_eq!(prompts[0].status, PromptStatus::Denied { scope: Scope::Once });
    }

    #[test]
    fn expired_event_flips_status() {
        let mut prompts = Vec::new();
        apply_event(&mut prompts, AccessEvent::Pending(pending("abc")));
        apply_event(
            &mut prompts,
            AccessEvent::Expired {
                id: "abc".into(),
                expired_at: Utc::now(),
            },
        );
        assert_eq!(prompts[0].status, PromptStatus::Expired);
    }

    #[test]
    fn resolved_for_unknown_id_is_no_op() {
        let mut prompts = Vec::new();
        // Resolving an id we never saw a Pending for must not panic /
        // synthesize a prompt. SSE may legitimately deliver Resolved
        // without Pending if the subscriber connected after the request
        // was already in flight.
        apply_event(
            &mut prompts,
            AccessEvent::Resolved(smooth_narc::AccessResolution {
                id: "never-seen".into(),
                verdict: ResolutionVerdict::Approve,
                scope: Scope::Once,
                glob_override: None,
                resolved_at: Utc::now(),
            }),
        );
        assert!(prompts.is_empty());
    }

    #[test]
    fn prompt_status_collapsed_label_shape() {
        assert!(PromptStatus::Open.collapsed_label().is_none());
        assert_eq!(
            PromptStatus::Approved {
                scope: Scope::Once,
                glob: None
            }
            .collapsed_label(),
            Some(("✓", "approved"))
        );
        assert_eq!(PromptStatus::Denied { scope: Scope::Once }.collapsed_label(), Some(("✗", "denied")));
        assert_eq!(PromptStatus::Expired.collapsed_label(), Some(("◌", "expired")));
    }
}
