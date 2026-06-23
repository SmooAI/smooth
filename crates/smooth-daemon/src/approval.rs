//! Bridges an in-agent-loop permission "ask" to the connected operator.
//!
//! When the [`PermissionHook`](crate::hook::PermissionHook) hits a
//! [`Decision::Ask`](crate::permission::Decision::Ask) it [`register`]s a
//! request and `await`s the returned receiver; the server resolves it when the
//! client sends a `PermissionReply`. If no one answers within the hook's
//! timeout, the hook fails closed (deny).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

/// Routes approval replies (by request id) back to waiting hooks.
#[derive(Debug, Default)]
pub struct ApprovalCoordinator {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalCoordinator {
    /// Create a shared coordinator.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<bool>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a pending approval; await the returned receiver for the verdict
    /// (`true` = approved).
    #[must_use]
    pub fn register(&self, request_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(request_id.to_owned(), tx);
        rx
    }

    /// Resolve a pending approval. Returns `false` if the id was unknown
    /// (already resolved or timed out).
    pub fn resolve(&self, request_id: &str, allow: bool) -> bool {
        // Remove first so the lock guard is dropped before we send.
        let removed = self.lock().remove(request_id);
        // `send` errs only if the receiver was already dropped (hook timed out).
        removed.is_some_and(|tx| tx.send(allow).is_ok())
    }

    /// Drop a pending request (e.g. on hook timeout) so it doesn't leak.
    pub fn forget(&self, request_id: &str) {
        self.lock().remove(request_id);
    }

    /// Number of in-flight approval requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_then_resolve_delivers_verdict() {
        let coord = ApprovalCoordinator::new();
        let rx = coord.register("r1");
        assert_eq!(coord.pending_count(), 1);
        assert!(coord.resolve("r1", true));
        assert!(rx.await.unwrap());
        assert_eq!(coord.pending_count(), 0);
    }

    #[tokio::test]
    async fn resolve_unknown_is_false() {
        let coord = ApprovalCoordinator::new();
        assert!(!coord.resolve("nope", true));
    }

    #[tokio::test]
    async fn forget_drops_the_request() {
        let coord = ApprovalCoordinator::new();
        let _rx = coord.register("r1");
        coord.forget("r1");
        assert_eq!(coord.pending_count(), 0);
        assert!(!coord.resolve("r1", false));
    }
}
