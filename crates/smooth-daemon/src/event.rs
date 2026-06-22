//! The durable event log — the spine of the daemon's state surface.
//!
//! Every observable thing the daemon does (a token streamed, a tool call
//! started, a task finished, an approval requested) becomes a [`DaemonEvent`]
//! with a **monotonic [`Seq`]**. Frontends subscribe to the `/api/event` SSE
//! stream and remember the last seq they saw; on reconnect they replay from
//! that cursor via [`EventStore::since`]. This closes the gap opencode left
//! stubbed (no server-side sequence resume), so a `th code` TUI or the React
//! SPA can drop its connection — or the daemon can restart — without losing
//! any state.
//!
//! Phase 0 ships the contract plus an in-memory implementation
//! ([`InMemoryEventLog`]) used by tests and dev. Phase 2 (th-bd0e22) adds a
//! Dolt-backed implementation of the same [`EventStore`] trait so the log
//! survives restarts; nothing above this trait changes.

use std::sync::Mutex;

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A monotonically increasing event sequence number, unique per daemon.
///
/// Seqs start at 1; a cursor of `0` means "from the beginning". The seq is the
/// resume cursor a frontend persists across reconnects.
pub type Seq = u64;

/// The payload of a [`DaemonEvent`].
///
/// These mirror the engine's `AgentEvent` stream at the wire boundary. Phase 0
/// defines the obvious variants plus a [`EventKind::Raw`] escape hatch so the
/// daemon can forward engine events that don't yet have a typed mapping; Phase
/// 1 (th-64fbe8) fills in the full `AgentEvent → EventKind` translation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A new agent turn began for the session.
    SessionStarted,
    /// An incremental chunk of assistant output.
    TokenDelta {
        /// The streamed text fragment.
        text: String,
    },
    /// The agent invoked a tool.
    ToolCallStarted {
        /// Engine-assigned tool-call id, correlating start with completion.
        call_id: String,
        /// Tool name (e.g. `bash`, `write`).
        name: String,
    },
    /// A previously started tool call finished.
    ToolCallCompleted {
        /// The id from the matching [`EventKind::ToolCallStarted`].
        call_id: String,
        /// Whether the tool succeeded.
        ok: bool,
    },
    /// The agent turn completed normally.
    TaskCompleted,
    /// The agent turn ended in an error.
    TaskFailed {
        /// Human-readable failure reason.
        message: String,
    },
    /// An engine event without a typed variant yet — forwarded verbatim.
    Raw {
        /// Opaque JSON payload.
        payload: serde_json::Value,
    },
}

/// A single sequenced, timestamped event in the daemon's durable log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonEvent {
    /// Monotonic sequence number assigned by the [`EventStore`] on append.
    pub seq: Seq,
    /// The session this event belongs to.
    pub session_id: String,
    /// When the event was appended (UTC).
    pub ts: DateTime<Utc>,
    /// The event payload.
    pub kind: EventKind,
}

/// Durable, append-only, monotonically-sequenced event log.
///
/// Implementations assign the [`Seq`] and timestamp on [`append`](EventStore::append);
/// callers never choose a seq. [`since`](EventStore::since) is the cursor-resume
/// primitive the SSE endpoint is built on.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append an event, returning the stored record with its assigned seq + ts.
    ///
    /// # Errors
    /// Returns an error if the underlying store cannot persist the event.
    async fn append(&self, session_id: &str, kind: EventKind) -> anyhow::Result<DaemonEvent>;

    /// Return events with `seq` strictly greater than `cursor`, in ascending
    /// seq order, capped at `limit`.
    ///
    /// When `session_id` is `Some`, only that session's events are returned
    /// (per-frontend session view); when `None`, the global stream is returned.
    ///
    /// # Errors
    /// Returns an error if the underlying store cannot be read.
    async fn since(&self, cursor: Seq, session_id: Option<&str>, limit: usize) -> anyhow::Result<Vec<DaemonEvent>>;

    /// The highest seq currently stored, or `0` if the log is empty.
    ///
    /// # Errors
    /// Returns an error if the underlying store cannot be read.
    async fn latest_seq(&self) -> anyhow::Result<Seq>;
}

/// In-memory [`EventStore`] — the dev/test backend.
///
/// Not durable across process restarts; Phase 2 swaps in a Dolt-backed
/// implementation behind the same trait.
#[derive(Debug, Default)]
pub struct InMemoryEventLog {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_seq: Seq,
    events: Vec<DaemonEvent>,
}

impl InMemoryEventLog {
    /// Create an empty in-memory log.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EventStore for InMemoryEventLog {
    async fn append(&self, session_id: &str, kind: EventKind) -> anyhow::Result<DaemonEvent> {
        let mut guard = self.inner.lock().map_err(|_| anyhow!("event log mutex poisoned"))?;
        // Seqs start at 1: a stored Inner::next_seq of 0 (Default) means empty.
        let seq = guard.next_seq + 1;
        guard.next_seq = seq;
        let event = DaemonEvent {
            seq,
            session_id: session_id.to_owned(),
            ts: Utc::now(),
            kind,
        };
        guard.events.push(event.clone());
        Ok(event)
    }

    async fn since(&self, cursor: Seq, session_id: Option<&str>, limit: usize) -> anyhow::Result<Vec<DaemonEvent>> {
        let guard = self.inner.lock().map_err(|_| anyhow!("event log mutex poisoned"))?;
        let out = guard
            .events
            .iter()
            .filter(|e| e.seq > cursor)
            .filter(|e| session_id.is_none_or(|sid| e.session_id == sid))
            .take(limit)
            .cloned()
            .collect();
        Ok(out)
    }

    async fn latest_seq(&self) -> anyhow::Result<Seq> {
        let guard = self.inner.lock().map_err(|_| anyhow!("event log mutex poisoned"))?;
        Ok(guard.next_seq)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn token(text: &str) -> EventKind {
        EventKind::TokenDelta { text: text.to_owned() }
    }

    #[tokio::test]
    async fn append_assigns_monotonic_seqs_starting_at_one() {
        let log = InMemoryEventLog::new();
        let a = log.append("s1", token("a")).await.unwrap();
        let b = log.append("s1", token("b")).await.unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(log.latest_seq().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn latest_seq_is_zero_when_empty() {
        let log = InMemoryEventLog::new();
        assert_eq!(log.latest_seq().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn since_returns_only_events_after_cursor_in_order() {
        let log = InMemoryEventLog::new();
        for c in ['a', 'b', 'c'] {
            log.append("s1", token(&c.to_string())).await.unwrap();
        }
        // Resume from seq 1 → should see seq 2 and 3 only.
        let tail = log.since(1, None, 100).await.unwrap();
        let seqs: Vec<Seq> = tail.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
    }

    #[tokio::test]
    async fn since_filters_by_session_when_requested() {
        let log = InMemoryEventLog::new();
        log.append("s1", token("a")).await.unwrap();
        log.append("s2", token("b")).await.unwrap();
        log.append("s1", token("c")).await.unwrap();

        let only_s1 = log.since(0, Some("s1"), 100).await.unwrap();
        let seqs: Vec<Seq> = only_s1.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 3], "only s1 events, global seqs preserved");

        let all = log.since(0, None, 100).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn since_respects_limit() {
        let log = InMemoryEventLog::new();
        for _ in 0..10 {
            log.append("s1", token("x")).await.unwrap();
        }
        let page = log.since(0, None, 4).await.unwrap();
        assert_eq!(page.len(), 4);
        let seqs: Vec<Seq> = page.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4], "limit takes the lowest unseen seqs first");
    }

    #[test]
    fn event_kind_serializes_internally_tagged() {
        let json = serde_json::to_value(EventKind::ToolCallStarted {
            call_id: "c1".into(),
            name: "bash".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "tool_call_started");
        assert_eq!(json["name"], "bash");
    }

    #[test]
    fn raw_event_round_trips() {
        let kind = EventKind::Raw {
            payload: serde_json::json!({"engine": "thing", "n": 7}),
        };
        let s = serde_json::to_string(&kind).unwrap();
        let back: EventKind = serde_json::from_str(&s).unwrap();
        assert_eq!(kind, back);
    }
}
