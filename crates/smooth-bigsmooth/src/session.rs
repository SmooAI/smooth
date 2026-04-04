//! Session management — persisting communication history with operators and resuming
//! interrupted orchestration.
//!
//! Provides [`SessionStore`] trait for persisting [`SessionMessage`] and [`OrchestratorSnapshot`]
//! records, an in-memory implementation ([`MemorySessionStore`]), and an [`Inbox`] for messages
//! awaiting human attention.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// The kind of message exchanged in a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageType {
    Command,
    Response,
    StatusUpdate,
    AccessRequest,
    Alert,
}

/// Lifecycle status of an orchestration session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum SessionStatus {
    Active,
    Paused,
    Interrupted,
    Completed,
    Failed,
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// A single message exchanged between Big Smooth and an operator (or vice-versa).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    /// `"bigsmooth"` or an operator id.
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: MessageType,
}

/// Point-in-time snapshot of an orchestration run — used for resuming interrupted work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSnapshot {
    pub session_id: String,
    pub bead_id: String,
    pub phase: String,
    pub operator_id: String,
    pub dispatched_at: DateTime<Utc>,
    pub last_checkpoint_id: Option<String>,
    pub status: SessionStatus,
}

// ---------------------------------------------------------------------------
// SessionStore trait
// ---------------------------------------------------------------------------

/// Persistence abstraction for session messages and orchestrator snapshots.
pub trait SessionStore: Send + Sync {
    /// Persist a message.
    fn save_message(&self, message: SessionMessage) -> anyhow::Result<()>;

    /// Retrieve the most recent `limit` messages for a session.
    fn get_messages(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<SessionMessage>>;

    /// Persist a snapshot.
    fn save_snapshot(&self, snapshot: OrchestratorSnapshot) -> anyhow::Result<()>;

    /// Retrieve the latest snapshot for a session.
    fn get_snapshot(&self, session_id: &str) -> anyhow::Result<Option<OrchestratorSnapshot>>;

    /// List all sessions whose latest snapshot has `Active` status.
    fn list_active_sessions(&self) -> anyhow::Result<Vec<OrchestratorSnapshot>>;

    /// Mark a session as [`SessionStatus::Completed`].
    fn mark_completed(&self, session_id: &str) -> anyhow::Result<()>;

    /// Mark a session as [`SessionStatus::Interrupted`].
    fn mark_interrupted(&self, session_id: &str) -> anyhow::Result<()>;
}

// ---------------------------------------------------------------------------
// MemorySessionStore
// ---------------------------------------------------------------------------

/// In-memory [`SessionStore`] implementation backed by `Mutex<Vec<…>>`.
#[derive(Debug, Default)]
pub struct MemorySessionStore {
    messages: Mutex<Vec<SessionMessage>>,
    snapshots: Mutex<Vec<OrchestratorSnapshot>>,
}

impl MemorySessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for MemorySessionStore {
    fn save_message(&self, message: SessionMessage) -> anyhow::Result<()> {
        self.messages.lock().expect("lock poisoned").push(message);
        Ok(())
    }

    fn get_messages(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<SessionMessage>> {
        let guard = self.messages.lock().expect("lock poisoned");
        let msgs: Vec<SessionMessage> = guard
            .iter()
            .rev()
            .filter(|m| m.session_id == session_id)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(msgs)
    }

    fn save_snapshot(&self, snapshot: OrchestratorSnapshot) -> anyhow::Result<()> {
        self.snapshots.lock().expect("lock poisoned").push(snapshot);
        Ok(())
    }

    fn get_snapshot(&self, session_id: &str) -> anyhow::Result<Option<OrchestratorSnapshot>> {
        let guard = self.snapshots.lock().expect("lock poisoned");
        Ok(guard.iter().rev().find(|s| s.session_id == session_id).cloned())
    }

    fn list_active_sessions(&self) -> anyhow::Result<Vec<OrchestratorSnapshot>> {
        let guard = self.snapshots.lock().expect("lock poisoned");
        // For each session_id, find the latest snapshot, keep it if Active.
        let mut latest: std::collections::HashMap<&str, &OrchestratorSnapshot> = std::collections::HashMap::new();
        for snap in &*guard {
            latest.insert(&snap.session_id, snap);
        }
        Ok(latest.into_values().filter(|s| s.status == SessionStatus::Active).cloned().collect())
    }

    fn mark_completed(&self, session_id: &str) -> anyhow::Result<()> {
        let mut guard = self.snapshots.lock().expect("lock poisoned");
        if let Some(snap) = guard.iter_mut().rev().find(|s| s.session_id == session_id) {
            snap.status = SessionStatus::Completed;
        }
        Ok(())
    }

    fn mark_interrupted(&self, session_id: &str) -> anyhow::Result<()> {
        let mut guard = self.snapshots.lock().expect("lock poisoned");
        if let Some(snap) = guard.iter_mut().rev().find(|s| s.session_id == session_id) {
            snap.status = SessionStatus::Interrupted;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

/// Queue of pending messages that need human attention (e.g. operator access requests).
#[derive(Debug, Default)]
pub struct Inbox {
    pending: Mutex<Vec<SessionMessage>>,
}

impl Inbox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message to the inbox.
    pub fn add_message(&self, message: SessionMessage) {
        self.pending.lock().expect("lock poisoned").push(message);
    }

    /// Return up to `limit` pending messages (oldest first).
    pub fn get_pending(&self, limit: usize) -> Vec<SessionMessage> {
        let guard = self.pending.lock().expect("lock poisoned");
        guard.iter().take(limit).cloned().collect()
    }

    /// Acknowledge (remove) a message by id. Returns `true` if found and removed.
    pub fn acknowledge(&self, message_id: &str) -> bool {
        let mut guard = self.pending.lock().expect("lock poisoned");
        if let Some(pos) = guard.iter().position(|m| m.id == message_id) {
            guard.remove(pos);
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(id: &str, session_id: &str, msg_type: MessageType) -> SessionMessage {
        SessionMessage {
            id: id.to_string(),
            session_id: session_id.to_string(),
            from: "bigsmooth".to_string(),
            to: "operator-1".to_string(),
            content: format!("content-{id}"),
            timestamp: Utc::now(),
            message_type: msg_type,
        }
    }

    fn make_snapshot(session_id: &str, status: SessionStatus) -> OrchestratorSnapshot {
        OrchestratorSnapshot {
            session_id: session_id.to_string(),
            bead_id: "bead-1".to_string(),
            phase: "Monitoring".to_string(),
            operator_id: "operator-1".to_string(),
            dispatched_at: Utc::now(),
            last_checkpoint_id: None,
            status,
        }
    }

    // --- SessionMessage ---

    #[test]
    fn session_message_creation() {
        let msg = make_message("m1", "s1", MessageType::Command);
        assert_eq!(msg.id, "m1");
        assert_eq!(msg.session_id, "s1");
        assert_eq!(msg.from, "bigsmooth");
        assert_eq!(msg.message_type, MessageType::Command);
    }

    #[test]
    fn session_message_serialization_roundtrip() {
        let msg = make_message("m2", "s1", MessageType::Response);
        let json = serde_json::to_string(&msg).unwrap();
        let deser: SessionMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.id, msg.id);
        assert_eq!(deser.content, msg.content);
        assert_eq!(deser.message_type, MessageType::Response);
    }

    // --- MessageType ---

    #[test]
    fn message_type_serialization() {
        for (variant, expected) in [
            (MessageType::Command, "\"Command\""),
            (MessageType::Response, "\"Response\""),
            (MessageType::StatusUpdate, "\"StatusUpdate\""),
            (MessageType::AccessRequest, "\"AccessRequest\""),
            (MessageType::Alert, "\"Alert\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let back: MessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // --- OrchestratorSnapshot ---

    #[test]
    fn orchestrator_snapshot_creation() {
        let snap = make_snapshot("s1", SessionStatus::Active);
        assert_eq!(snap.session_id, "s1");
        assert_eq!(snap.status, SessionStatus::Active);
        assert!(snap.last_checkpoint_id.is_none());
    }

    #[test]
    fn orchestrator_snapshot_serialization_roundtrip() {
        let mut snap = make_snapshot("s2", SessionStatus::Paused);
        snap.last_checkpoint_id = Some("cp-42".to_string());
        let json = serde_json::to_string(&snap).unwrap();
        let deser: OrchestratorSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.session_id, "s2");
        assert_eq!(deser.status, SessionStatus::Paused);
        assert_eq!(deser.last_checkpoint_id.as_deref(), Some("cp-42"));
    }

    // --- SessionStatus ordering ---

    #[test]
    fn session_status_ordering() {
        assert!(SessionStatus::Active < SessionStatus::Paused);
        assert!(SessionStatus::Paused < SessionStatus::Interrupted);
        assert!(SessionStatus::Interrupted < SessionStatus::Completed);
        assert!(SessionStatus::Completed < SessionStatus::Failed);
    }

    // --- MemorySessionStore ---

    #[test]
    fn store_save_and_get_messages() {
        let store = MemorySessionStore::new();
        store.save_message(make_message("m1", "s1", MessageType::Command)).unwrap();
        store.save_message(make_message("m2", "s1", MessageType::Response)).unwrap();
        store.save_message(make_message("m3", "s2", MessageType::Alert)).unwrap();

        let msgs = store.get_messages("s1", 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, "m1");
        assert_eq!(msgs[1].id, "m2");
    }

    #[test]
    fn store_get_messages_respects_limit() {
        let store = MemorySessionStore::new();
        for i in 0..5 {
            store.save_message(make_message(&format!("m{i}"), "s1", MessageType::StatusUpdate)).unwrap();
        }
        let msgs = store.get_messages("s1", 3).unwrap();
        assert_eq!(msgs.len(), 3);
        // Should be the 3 most recent
        assert_eq!(msgs[0].id, "m2");
        assert_eq!(msgs[2].id, "m4");
    }

    #[test]
    fn store_save_and_get_snapshot() {
        let store = MemorySessionStore::new();
        store.save_snapshot(make_snapshot("s1", SessionStatus::Active)).unwrap();
        let snap = store.get_snapshot("s1").unwrap().unwrap();
        assert_eq!(snap.session_id, "s1");
        assert_eq!(snap.status, SessionStatus::Active);

        assert!(store.get_snapshot("nonexistent").unwrap().is_none());
    }

    #[test]
    fn store_list_active_sessions() {
        let store = MemorySessionStore::new();
        store.save_snapshot(make_snapshot("s1", SessionStatus::Active)).unwrap();
        store.save_snapshot(make_snapshot("s2", SessionStatus::Completed)).unwrap();
        store.save_snapshot(make_snapshot("s3", SessionStatus::Active)).unwrap();

        let active = store.list_active_sessions().unwrap();
        assert_eq!(active.len(), 2);
        let ids: Vec<&str> = active.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"s1"));
        assert!(ids.contains(&"s3"));
    }

    #[test]
    fn store_mark_completed() {
        let store = MemorySessionStore::new();
        store.save_snapshot(make_snapshot("s1", SessionStatus::Active)).unwrap();
        store.mark_completed("s1").unwrap();
        let snap = store.get_snapshot("s1").unwrap().unwrap();
        assert_eq!(snap.status, SessionStatus::Completed);
    }

    #[test]
    fn store_mark_interrupted() {
        let store = MemorySessionStore::new();
        store.save_snapshot(make_snapshot("s1", SessionStatus::Active)).unwrap();
        store.mark_interrupted("s1").unwrap();
        let snap = store.get_snapshot("s1").unwrap().unwrap();
        assert_eq!(snap.status, SessionStatus::Interrupted);
    }

    // --- Inbox ---

    #[test]
    fn inbox_add_and_get_pending() {
        let inbox = Inbox::new();
        inbox.add_message(make_message("i1", "s1", MessageType::AccessRequest));
        inbox.add_message(make_message("i2", "s1", MessageType::AccessRequest));

        let pending = inbox.get_pending(10);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, "i1");
    }

    #[test]
    fn inbox_get_pending_respects_limit() {
        let inbox = Inbox::new();
        for i in 0..5 {
            inbox.add_message(make_message(&format!("i{i}"), "s1", MessageType::Alert));
        }
        let pending = inbox.get_pending(2);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, "i0");
        assert_eq!(pending[1].id, "i1");
    }

    #[test]
    fn inbox_acknowledge() {
        let inbox = Inbox::new();
        inbox.add_message(make_message("i1", "s1", MessageType::AccessRequest));
        inbox.add_message(make_message("i2", "s1", MessageType::AccessRequest));

        assert!(inbox.acknowledge("i1"));
        assert!(!inbox.acknowledge("i1")); // already removed

        let pending = inbox.get_pending(10);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "i2");
    }
}
