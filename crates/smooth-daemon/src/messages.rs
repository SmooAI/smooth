//! Durable conversation history per session.
//!
//! The engine randomizes an `Agent`'s id every turn, so cross-turn (and
//! cross-restart) conversation continuity is done by **replaying prior
//! messages** into a fresh agent (`AgentConfig::with_prior_messages`), not by
//! checkpoint-by-id. The [`MessageStore`] persists each completed turn's user +
//! assistant messages so the daemon can reload a session's history and continue
//! the conversation after a restart.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A stored conversation message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Durable per-session conversation history (append-only, ordered).
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Append a message to a session's history.
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn append(&self, session_id: &str, role: &str, content: &str) -> anyhow::Result<()>;

    /// Load a session's history, oldest-first, capped at `limit`.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read.
    async fn load(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<StoredMessage>>;
}

/// In-memory [`MessageStore`] — dev/test backend.
#[derive(Debug, Default)]
pub struct InMemoryMessageStore {
    inner: Mutex<HashMap<String, Vec<StoredMessage>>>,
}

impl InMemoryMessageStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MessageStore for InMemoryMessageStore {
    async fn append(&self, session_id: &str, role: &str, content: &str) -> anyhow::Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.entry(session_id.to_owned()).or_default().push(StoredMessage {
            role: role.to_owned(),
            content: content.to_owned(),
        });
        Ok(())
    }

    async fn load(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<StoredMessage>> {
        let guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard.get(session_id).map(|v| v.iter().take(limit).cloned().collect()).unwrap_or_default())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_load_preserves_order() {
        let store = InMemoryMessageStore::new();
        store.append("s1", "user", "hi").await.unwrap();
        store.append("s1", "assistant", "hello").await.unwrap();
        store.append("s2", "user", "other").await.unwrap();

        let s1 = store.load("s1", 100).await.unwrap();
        assert_eq!(
            s1,
            vec![
                StoredMessage {
                    role: "user".into(),
                    content: "hi".into()
                },
                StoredMessage {
                    role: "assistant".into(),
                    content: "hello".into()
                },
            ]
        );
        assert_eq!(store.load("s2", 100).await.unwrap().len(), 1);
        assert_eq!(store.load("missing", 100).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn load_respects_limit() {
        let store = InMemoryMessageStore::new();
        for i in 0..5 {
            store.append("s1", "user", &i.to_string()).await.unwrap();
        }
        assert_eq!(store.load("s1", 3).await.unwrap().len(), 3);
    }
}
