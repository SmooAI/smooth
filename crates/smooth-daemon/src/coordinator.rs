//! Per-session run coordination.
//!
//! Borrowed from opencode's `SessionRunCoordinator`: **at most one agent fiber
//! runs per session at a time, but sessions run concurrently**. A session is
//! the unit of conversational serialization — two `TaskStart`s for the same
//! session must not interleave turns on the same conversation, while unrelated
//! sessions proceed in parallel.
//!
//! This type is deliberately agnostic to the agent: it spawns an opaque
//! `Future` per session and tracks an [`AbortHandle`] so a `TaskCancel` can
//! stop it. The engine wiring lives in the server (Stage B); this keeps the
//! concurrency contract pure and unit-testable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use std::future::Future;
use tokio::task::AbortHandle;

/// Why a [`SessionRunCoordinator::try_start`] was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StartError {
    /// The session already has a running task; the caller should surface this
    /// (e.g. "session busy — cancel or steer the current task first") rather
    /// than silently interleaving turns.
    #[error("session {session_id} already has a running task ({task_id})")]
    Busy {
        /// The busy session.
        session_id: String,
        /// The task currently occupying it.
        task_id: String,
    },
}

struct RunningHandle {
    task_id: String,
    abort: AbortHandle,
}

/// Tracks the single in-flight task per session.
#[derive(Default)]
pub struct SessionRunCoordinator {
    running: Mutex<HashMap<String, RunningHandle>>,
}

impl SessionRunCoordinator {
    /// Create an empty coordinator.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, RunningHandle>> {
        // A poisoned lock means a prior holder panicked mid-mutation. The map is
        // just a registry of handles; recovering the inner guard is safe and
        // keeps the daemon alive (we never leave it in a half-updated state
        // across an await — all critical sections below are synchronous).
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start `fut` as the session's task, unless one is already running.
    ///
    /// On natural completion the session is freed automatically. Different
    /// sessions run concurrently. Returns [`StartError::Busy`] if `session_id`
    /// is already occupied.
    ///
    /// # Errors
    /// Returns [`StartError::Busy`] when the session already has a running task.
    pub fn try_start<F>(self: &Arc<Self>, session_id: impl Into<String>, task_id: impl Into<String>, fut: F) -> Result<(), StartError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let session_id = session_id.into();
        let task_id = task_id.into();

        let mut map = self.lock();
        if let Some(existing) = map.get(&session_id) {
            return Err(StartError::Busy {
                session_id,
                task_id: existing.task_id.clone(),
            });
        }

        let this = Arc::clone(self);
        let sid = session_id.clone();
        let tid = task_id.clone();
        let join = tokio::spawn(async move {
            fut.await;
            // Free the slot, but only if we still own it (a cancel + new start
            // could have replaced us — guard on task id).
            this.finish(&sid, &tid);
        });

        map.insert(
            session_id,
            RunningHandle {
                task_id,
                abort: join.abort_handle(),
            },
        );
        Ok(())
    }

    /// Whether `session_id` currently has a running task.
    #[must_use]
    pub fn is_busy(&self, session_id: &str) -> bool {
        self.lock().contains_key(session_id)
    }

    /// The task id currently running for `session_id`, if any.
    #[must_use]
    pub fn current_task(&self, session_id: &str) -> Option<String> {
        self.lock().get(session_id).map(|h| h.task_id.clone())
    }

    /// Number of sessions with a running task.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.lock().len()
    }

    /// Cancel the task running for `session_id`, if any. Returns the cancelled
    /// task id. The aborted future does not run its completion cleanup, so this
    /// frees the slot directly.
    pub fn cancel_session(&self, session_id: &str) -> Option<String> {
        let mut map = self.lock();
        map.remove(session_id).map(|h| {
            h.abort.abort();
            h.task_id
        })
    }

    /// Cancel by task id, regardless of which session it belongs to.
    pub fn cancel_task(&self, task_id: &str) -> bool {
        let mut map = self.lock();
        let Some(session_id) = map.iter().find(|(_, h)| h.task_id == task_id).map(|(s, _)| s.clone()) else {
            return false;
        };
        if let Some(h) = map.remove(&session_id) {
            h.abort.abort();
            return true;
        }
        false
    }

    /// Remove the session's slot iff the running task id still matches — called
    /// by a task when it finishes naturally.
    fn finish(&self, session_id: &str, task_id: &str) {
        let mut map = self.lock();
        if map.get(session_id).is_some_and(|h| h.task_id == task_id) {
            map.remove(session_id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn different_sessions_run_concurrently() {
        let coord = SessionRunCoordinator::new();
        let gate = Arc::new(Notify::new());

        for (s, t) in [("s1", "t1"), ("s2", "t2")] {
            let g = Arc::clone(&gate);
            coord.try_start(s, t, async move { g.notified().await }).unwrap();
        }
        assert_eq!(coord.active_count(), 2, "both sessions active in parallel");
        assert!(coord.is_busy("s1") && coord.is_busy("s2"));

        // Release both tasks and let them drain.
        gate.notify_waiters();
    }

    #[tokio::test]
    async fn same_session_is_serialized() {
        let coord = SessionRunCoordinator::new();
        let gate = Arc::new(Notify::new());

        let g = Arc::clone(&gate);
        coord.try_start("s1", "t1", async move { g.notified().await }).unwrap();

        let err = coord.try_start("s1", "t2", async {}).unwrap_err();
        assert_eq!(
            err,
            StartError::Busy {
                session_id: "s1".into(),
                task_id: "t1".into()
            },
            "second task on a busy session is rejected, not interleaved"
        );

        gate.notify_waiters();
    }

    #[tokio::test]
    async fn slot_frees_after_natural_completion() {
        let coord = SessionRunCoordinator::new();
        coord.try_start("s1", "t1", async {}).unwrap();

        // The completion cleanup runs on the spawned task; give it a moment.
        for _ in 0..50 {
            if !coord.is_busy("s1") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(!coord.is_busy("s1"), "session freed after task completed");
        // And can be reused.
        coord.try_start("s1", "t3", async {}).unwrap();
    }

    #[tokio::test]
    async fn cancel_session_aborts_and_frees() {
        let coord = SessionRunCoordinator::new();
        let gate = Arc::new(Notify::new());
        let g = Arc::clone(&gate);
        coord.try_start("s1", "t1", async move { g.notified().await }).unwrap();

        assert_eq!(coord.cancel_session("s1"), Some("t1".to_string()));
        assert!(!coord.is_busy("s1"), "cancelled session is freed immediately");
        assert_eq!(coord.cancel_session("s1"), None, "no-op cancel on idle session");
    }

    #[tokio::test]
    async fn cancel_task_by_id_finds_the_right_session() {
        let coord = SessionRunCoordinator::new();
        let gate = Arc::new(Notify::new());
        for (s, t) in [("s1", "t1"), ("s2", "t2")] {
            let g = Arc::clone(&gate);
            coord.try_start(s, t, async move { g.notified().await }).unwrap();
        }
        assert!(coord.cancel_task("t2"));
        assert!(!coord.is_busy("s2"));
        assert!(coord.is_busy("s1"), "cancelling t2 left s1 untouched");
        assert!(!coord.cancel_task("nope"), "unknown task id is a no-op");

        gate.notify_waiters();
    }
}
