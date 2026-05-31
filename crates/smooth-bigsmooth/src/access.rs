//! Access request store — the in-process queue + broadcast that backs the
//! Claude-Code-style auto-mode permission model.
//!
//! When Safehouse Narc decides [`smooth_narc::judge::Decision::Ask`] (or the
//! legacy `EscalateToHuman`), the request is filed here. The TUI (or any
//! other UI) can:
//!
//! - List pending requests via `list_pending()` (used by `GET
//!   /api/access/pending` and `th access pending`).
//! - Subscribe to `events()` to get a real-time push of new pending
//!   requests + resolutions (used by the SSE stream and the TUI's inline
//!   approval card).
//! - Resolve a request via `resolve()` (used by `POST /api/access/approve`
//!   / `/api/access/deny`). The caller side of `file_pending()` was handed
//!   back a future on the resolution — resolving wakes that future so the
//!   originating tool call can retry against the resolved verdict.
//!
//! ## Lifecycle
//!
//! ```text
//!   Wonk ─► Narc ─► Decision::Ask
//!                       │
//!                       ▼
//!                  AccessStore::file_pending(req) ────► (id, AccessResolutionFuture)
//!                       │                                       │
//!                       │ broadcast Pending                     │ caller awaits
//!                       ▼                                       │
//!                       SSE subscribers                         │
//!                                                               │
//!   Human resolves ──► POST /api/access/approve { id, scope } ──┤
//!                                                               │
//!                       AccessStore::resolve(...) ──► oneshot ──┘
//!                       │
//!                       │ broadcast Resolved
//!                       ▼
//!                       SSE subscribers
//! ```
//!
//! ## Why an in-process store
//!
//! Pending state is intentionally not persisted to disk. A pending request
//! is bound to a live tool call; if Big Smooth restarts, the call is dead
//! anyway. Persistent grants (scope `PearlProject` / `User`) get written to
//! `wonk-allow.toml` files by the resolve handler, not by this store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use smooth_narc::judge::Scope;
use tokio::sync::{broadcast, oneshot};

// Re-export the wire types from smooth-narc so existing callers of
// `crate::access::PendingAccessRequest` (HTTP handlers, tests) keep
// working without an import change. The wire types live in
// smooth-narc so the TUI / CLI can consume them without depending on
// this crate.
pub use smooth_narc::access_wire::{AccessEvent, AccessKind, AccessResolution, NewAccessRequest, PendingAccessRequest, ResolutionVerdict};

/// Broadcast channel capacity. Larger than typical concurrent subscriber
/// count so a slow SSE consumer can't easily drop events; if a subscriber
/// does fall behind, broadcast::Receiver::recv returns Lagged and the
/// subscriber re-syncs by re-fetching the pending list.
const BROADCAST_CAPACITY: usize = 256;

/// Errors from the access store.
#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("no pending access request with id {0}")]
    NotFound(String),
    #[error("access store mutex was poisoned")]
    Poisoned,
}

/// A handle to the in-process access request store. Cheap to clone (Arc'd).
#[derive(Clone, Default)]
pub struct AccessStore {
    inner: Arc<Inner>,
}

struct Inner {
    pending: Mutex<HashMap<String, PendingState>>,
    events: broadcast::Sender<AccessEvent>,
}

impl Default for Inner {
    fn default() -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
        }
    }
}

struct PendingState {
    request: PendingAccessRequest,
    resolver: Option<oneshot::Sender<AccessResolution>>,
}

/// Future-like handle returned from `file_pending`. Awaiting it yields the
/// resolution, or `None` if the request was expired/dropped before the
/// human resolved it.
pub struct AccessResolutionFuture(oneshot::Receiver<AccessResolution>);

impl AccessResolutionFuture {
    /// Await the resolution. Returns `None` if the request was dropped or
    /// expired without a resolution.
    pub async fn await_resolution(self) -> Option<AccessResolution> {
        self.0.await.ok()
    }

    /// Await with a timeout. Returns `None` on timeout or drop.
    pub async fn await_resolution_with_timeout(self, timeout: Duration) -> Option<AccessResolution> {
        tokio::time::timeout(timeout, self.0).await.ok().and_then(std::result::Result::ok)
    }
}

impl AccessStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// File a new pending request. Returns `(id, future)` — the caller
    /// keeps the future and awaits it; resolving the request (via
    /// `resolve()`) wakes the future.
    pub fn file_pending(&self, req: NewAccessRequest) -> (String, AccessResolutionFuture) {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let request = PendingAccessRequest {
            id: id.clone(),
            bead_id: req.bead_id,
            operator_id: req.operator_id,
            kind: req.kind,
            resource: req.resource,
            detail: req.detail,
            reason: req.reason,
            scope_options: req.scope_options,
            created_at: Utc::now(),
        };

        // Insert. A poisoned mutex means a prior caller panicked — we
        // recover and continue rather than escalate the panic, since the
        // store is purely in-memory state with no persistence boundary
        // to worry about.
        {
            let mut pending = self.inner.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.insert(
                id.clone(),
                PendingState {
                    request: request.clone(),
                    resolver: Some(tx),
                },
            );
        }

        // Broadcast. Errors here only happen when there are no subscribers,
        // which is fine — the request still lives in the pending map.
        let _ = self.inner.events.send(AccessEvent::Pending(request));

        (id, AccessResolutionFuture(rx))
    }

    /// Resolve a pending request. Wakes the waiter and broadcasts a
    /// `Resolved` event. Errors if the id isn't pending.
    pub fn resolve(&self, id: &str, verdict: ResolutionVerdict, scope: Scope, glob_override: Option<String>) -> Result<AccessResolution, AccessError> {
        let mut pending = self.inner.pending.lock().map_err(|_| AccessError::Poisoned)?;
        let Some(mut state) = pending.remove(id) else {
            return Err(AccessError::NotFound(id.to_string()));
        };
        let resolution = AccessResolution {
            id: id.to_string(),
            verdict,
            scope,
            glob_override,
            resolved_at: Utc::now(),
        };
        // Drop the lock before sending so a slow subscriber can't deadlock
        // a future caller of `resolve`.
        drop(pending);

        if let Some(tx) = state.resolver.take() {
            // If the receiver was dropped (caller gave up), this errors —
            // not fatal, we still broadcast the resolution for SSE
            // subscribers + audit.
            let _ = tx.send(resolution.clone());
        }
        let _ = self.inner.events.send(AccessEvent::Resolved(resolution.clone()));
        Ok(resolution)
    }

    /// Expire a request without resolution. Wakes the waiter with `None`
    /// (the oneshot sender is dropped). Broadcasts an `Expired` event.
    pub fn expire(&self, id: &str) -> Result<(), AccessError> {
        let mut pending = self.inner.pending.lock().map_err(|_| AccessError::Poisoned)?;
        if pending.remove(id).is_none() {
            return Err(AccessError::NotFound(id.to_string()));
        }
        drop(pending);
        let _ = self.inner.events.send(AccessEvent::Expired {
            id: id.to_string(),
            expired_at: Utc::now(),
        });
        Ok(())
    }

    /// Snapshot of all currently-pending requests, sorted by `created_at`
    /// ascending (oldest first).
    #[must_use]
    pub fn list_pending(&self) -> Vec<PendingAccessRequest> {
        let pending = self.inner.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut requests: Vec<_> = pending.values().map(|s| s.request.clone()).collect();
        requests.sort_by_key(|r| r.created_at);
        requests
    }

    /// Number of pending requests. For diagnostics.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        let pending = self.inner.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.len()
    }

    /// Subscribe to access events. The returned receiver yields every
    /// future Pending/Resolved/Expired event. The caller should re-sync
    /// via `list_pending()` on first subscribe to catch up.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<AccessEvent> {
        self.inner.events.subscribe()
    }

    /// Number of live broadcast receivers — the SSE handler each time
    /// it accepts a connection, plus anything else that called
    /// [`AccessStore::subscribe`]. Used by integration tests to wait
    /// until the wire-side subscriber has actually registered before
    /// firing a broadcast that would otherwise be dropped (broadcast
    /// channels deliver only to receivers that exist at send time).
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.inner.events.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_test_request(store: &AccessStore) -> (String, AccessResolutionFuture) {
        let mut req = NewAccessRequest::with_defaults("pearl-1", "op-1", "network", "api.example.com", "domain not in allowlist");
        req.detail = Some("GET /v1/models".into());
        store.file_pending(req)
    }

    #[tokio::test]
    async fn file_and_resolve_wakes_waiter() {
        let store = AccessStore::new();
        let (id, fut) = file_test_request(&store);
        // Resolution flows: list shows it, resolve clears it, future fires.
        assert_eq!(store.pending_count(), 1);
        let _ = store.resolve(&id, ResolutionVerdict::Approve, Scope::Session, None).expect("resolve");
        assert_eq!(store.pending_count(), 0);
        let resolution = fut.await_resolution().await.expect("resolution delivered");
        assert_eq!(resolution.verdict, ResolutionVerdict::Approve);
        assert_eq!(resolution.scope, Scope::Session);
    }

    #[tokio::test]
    async fn expire_wakes_waiter_with_none() {
        let store = AccessStore::new();
        let (id, fut) = file_test_request(&store);
        store.expire(&id).expect("expire");
        // Dropped oneshot sender → None.
        assert!(fut.await_resolution().await.is_none());
        assert_eq!(store.pending_count(), 0);
    }

    #[tokio::test]
    async fn resolve_unknown_id_errors() {
        let store = AccessStore::new();
        let err = store
            .resolve("never-filed", ResolutionVerdict::Approve, Scope::Once, None)
            .expect_err("not found");
        assert!(matches!(err, AccessError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_pending_sorted_oldest_first() {
        let store = AccessStore::new();
        let (id1, _f1) = file_test_request(&store);
        // Sleep so created_at advances at least a microsecond.
        tokio::time::sleep(Duration::from_millis(2)).await;
        let (id2, _f2) = file_test_request(&store);

        let list = store.list_pending();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, id1);
        assert_eq!(list[1].id, id2);
    }

    #[tokio::test]
    async fn subscribe_receives_pending_and_resolved() {
        let store = AccessStore::new();
        let mut rx = store.subscribe();
        let (id, fut) = file_test_request(&store);

        // Subscriber should see the Pending event first.
        let evt = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("pending event")
            .expect("recv ok");
        assert!(matches!(evt, AccessEvent::Pending(_)));

        let _ = store.resolve(&id, ResolutionVerdict::Deny, Scope::Once, None).expect("resolve");
        let evt = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("resolved event")
            .expect("recv ok");
        match evt {
            AccessEvent::Resolved(r) => {
                assert_eq!(r.verdict, ResolutionVerdict::Deny);
                assert_eq!(r.scope, Scope::Once);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }

        // The waiter also got the resolution.
        let resolution = fut.await_resolution().await.expect("resolution");
        assert_eq!(resolution.verdict, ResolutionVerdict::Deny);
    }

    #[tokio::test]
    async fn timeout_returns_none_but_request_still_pending() {
        let store = AccessStore::new();
        let (id, fut) = file_test_request(&store);
        // Wait with a short timeout — no resolver, so the future stays
        // pending until the timeout fires.
        let result = fut.await_resolution_with_timeout(Duration::from_millis(10)).await;
        assert!(result.is_none());
        // The request is still pending in the store — timeouts don't
        // implicitly expire. The caller must expire() explicitly.
        assert_eq!(store.pending_count(), 1);
        // We can still resolve it (though the original waiter is gone).
        let resolution = store
            .resolve(&id, ResolutionVerdict::Approve, Scope::Once, None)
            .expect("resolve after timeout");
        assert_eq!(resolution.verdict, ResolutionVerdict::Approve);
    }

    #[tokio::test]
    async fn glob_override_round_trips() {
        let store = AccessStore::new();
        let (id, _fut) = file_test_request(&store);
        let resolution = store
            .resolve(&id, ResolutionVerdict::Approve, Scope::PearlProject, Some("*.example.com".into()))
            .expect("resolve");
        assert_eq!(resolution.glob_override.as_deref(), Some("*.example.com"));
    }

    #[test]
    fn access_event_serde_uses_tagged_form() {
        // Wire format is { "event": "pending", ... } — the TUI parses
        // events by inspecting the event tag.
        let evt = AccessEvent::Pending(PendingAccessRequest {
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
        let json = serde_json::to_string(&evt).expect("serialize");
        assert!(json.contains("\"event\":\"pending\""));
        assert!(json.contains("\"scope_options\":[\"once\"]"));
    }

    #[test]
    fn resolution_verdict_as_str() {
        assert_eq!(ResolutionVerdict::Approve.as_str(), "approve");
        assert_eq!(ResolutionVerdict::Deny.as_str(), "deny");
    }
}
