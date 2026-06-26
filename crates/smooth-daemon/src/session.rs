//! Session registry.
//!
//! A *session* is the daemon's unit of conversational identity — the key the
//! [`SessionRunCoordinator`](crate::coordinator::SessionRunCoordinator)
//! serializes on and the [`EventStore`](crate::event::EventStore) tags events
//! with. The [`SessionStore`] tracks session metadata (title, timestamps,
//! status) so the control surface can list and resume conversations.
//!
//! Phase 1 ships the trait + an in-memory implementation. Phase 2 (th-bd0e22)
//! adds a Dolt-backed implementation behind the same trait, at which point
//! sessions — and their resume — survive a daemon restart.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle state of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Has a task running right now.
    Active,
    /// Exists, no task currently running.
    Idle,
    /// Explicitly ended.
    Completed,
}

/// Session metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Stable session id (the WS/coordinator/event key).
    pub id: String,
    /// Optional human label.
    pub title: Option<String>,
    /// When the session was first created (UTC).
    pub created_at: DateTime<Utc>,
    /// Last activity (UTC).
    pub updated_at: DateTime<Utc>,
    /// Lifecycle state.
    pub status: SessionStatus,
}

/// Persistent-ish registry of sessions.
///
/// `async` so the Dolt-backed Phase 2 implementation slots in unchanged.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Create a session, or return the existing one if `id` is already present
    /// (idempotent — a reconnecting client passing its id resumes the row).
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn create(&self, id: Option<String>, title: Option<String>) -> anyhow::Result<Session>;

    /// Fetch a session by id.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read.
    async fn get(&self, id: &str) -> anyhow::Result<Option<Session>>;

    /// List sessions, most-recently-updated first.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read.
    async fn list(&self) -> anyhow::Result<Vec<Session>>;

    /// Bump a session's `updated_at` to now (no-op if unknown).
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn touch(&self, id: &str) -> anyhow::Result<()>;

    /// Set a session's status (no-op if unknown).
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn set_status(&self, id: &str, status: SessionStatus) -> anyhow::Result<()>;

    /// Set a session's title **only if it currently has none** (no-op if unknown
    /// or already titled). Used to auto-title a session from its first message
    /// without clobbering a title the operator chose explicitly.
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn set_title_if_unset(&self, id: &str, title: &str) -> anyhow::Result<()>;
}

/// In-memory [`SessionStore`] — the dev/test backend (not durable).
#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    inner: Mutex<HashMap<String, Session>>,
}

impl InMemorySessionStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create(&self, id: Option<String>, title: Option<String>) -> anyhow::Result<Session> {
        let now = Utc::now();
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut map = self.lock();
        if let Some(existing) = map.get(&id) {
            return Ok(existing.clone());
        }
        let session = Session {
            id: id.clone(),
            title,
            created_at: now,
            updated_at: now,
            status: SessionStatus::Idle,
        };
        map.insert(id, session.clone());
        Ok(session)
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<Session>> {
        Ok(self.lock().get(id).cloned())
    }

    async fn list(&self) -> anyhow::Result<Vec<Session>> {
        let mut sessions: Vec<Session> = self.lock().values().cloned().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(sessions)
    }

    async fn touch(&self, id: &str) -> anyhow::Result<()> {
        if let Some(s) = self.lock().get_mut(id) {
            s.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn set_status(&self, id: &str, status: SessionStatus) -> anyhow::Result<()> {
        if let Some(s) = self.lock().get_mut(id) {
            s.status = status;
            s.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn set_title_if_unset(&self, id: &str, title: &str) -> anyhow::Result<()> {
        if let Some(s) = self.lock().get_mut(id) {
            if s.title.as_deref().is_none_or(str::is_empty) {
                s.title = Some(title.to_owned());
                s.updated_at = Utc::now();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_is_idempotent_on_explicit_id() {
        let store = InMemorySessionStore::new();
        let a = store.create(Some("s1".into()), Some("first".into())).await.unwrap();
        let b = store.create(Some("s1".into()), Some("ignored".into())).await.unwrap();
        assert_eq!(a, b, "second create with same id returns the original row");
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert_eq!(a.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn create_without_id_generates_one() {
        let store = InMemorySessionStore::new();
        let s = store.create(None, None).await.unwrap();
        assert!(!s.id.is_empty());
        assert_eq!(store.get(&s.id).await.unwrap(), Some(s));
    }

    #[tokio::test]
    async fn set_title_if_unset_fills_blank_but_keeps_explicit() {
        let store = InMemorySessionStore::new();
        // Untitled session gets auto-titled.
        store.create(Some("blank".into()), None).await.unwrap();
        store.set_title_if_unset("blank", "auto title").await.unwrap();
        assert_eq!(store.get("blank").await.unwrap().unwrap().title.as_deref(), Some("auto title"));
        // A second call does not overwrite the now-set title.
        store.set_title_if_unset("blank", "later").await.unwrap();
        assert_eq!(store.get("blank").await.unwrap().unwrap().title.as_deref(), Some("auto title"));
        // An explicitly-titled session is left alone.
        store.create(Some("named".into()), Some("chosen".into())).await.unwrap();
        store.set_title_if_unset("named", "auto").await.unwrap();
        assert_eq!(store.get("named").await.unwrap().unwrap().title.as_deref(), Some("chosen"));
    }

    #[tokio::test]
    async fn set_status_and_touch_update_the_row() {
        let store = InMemorySessionStore::new();
        let s = store.create(Some("s1".into()), None).await.unwrap();
        store.set_status("s1", SessionStatus::Active).await.unwrap();
        let after = store.get("s1").await.unwrap().unwrap();
        assert_eq!(after.status, SessionStatus::Active);
        assert!(after.updated_at >= s.updated_at);

        // Unknown id is a no-op, not an error.
        store.touch("nope").await.unwrap();
        store.set_status("nope", SessionStatus::Idle).await.unwrap();
    }

    #[tokio::test]
    async fn list_is_newest_first() {
        let store = InMemorySessionStore::new();
        store.create(Some("old".into()), None).await.unwrap();
        store.create(Some("new".into()), None).await.unwrap();
        // Touch "new" so it's strictly latest.
        store.touch("new").await.unwrap();
        let ids: Vec<String> = store.list().await.unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids.first().map(String::as_str), Some("new"));
    }

    #[test]
    fn status_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&SessionStatus::Active).unwrap(), "\"active\"");
    }
}
