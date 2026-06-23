//! SQLite-backed durable persistence for the daemon's runtime state.
//!
//! The daemon's events and sessions are **per-instance runtime state**, not
//! team-synced work items — so they live in a local SQLite database (WAL mode),
//! not Dolt. (Dolt's version-control + `refs/dolt/data` sync is for *pearls*.)
//! rusqlite ships a bundled SQLite, so there is no external binary and tests
//! run anywhere.
//!
//! These implement the same [`EventStore`](crate::event::EventStore) and
//! [`SessionStore`](crate::session::SessionStore) traits as the in-memory
//! backends, so swapping them in (via [`open_stores`]) makes the SSE
//! cursor-resume stream and the `/api/session` list survive a daemon restart
//! with zero changes above the trait — the headline Phase 2 capability.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use smooth_operator::{Memory, MemoryEntry, MemoryType};

use crate::event::{DaemonEvent, EventKind, EventStore, Seq};
use crate::messages::{MessageStore, StoredMessage};
use crate::schedule::{Schedule, ScheduleKind, ScheduleStore};
use crate::session::{Session, SessionStatus, SessionStore};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    ts         TEXT NOT NULL,
    kind       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events(session_id, seq);

CREATE TABLE IF NOT EXISTS sessions (
    id         TEXT PRIMARY KEY,
    title      TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    status     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS session_messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_msg_session ON session_messages(session_id, id);

CREATE TABLE IF NOT EXISTS memories (
    id            TEXT PRIMARY KEY,
    content       TEXT NOT NULL,
    memory_type   TEXT NOT NULL,
    relevance     REAL NOT NULL,
    metadata      TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_accessed TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS schedules (
    id        TEXT PRIMARY KEY,
    prompt    TEXT NOT NULL,
    kind      TEXT NOT NULL,
    enabled   INTEGER NOT NULL,
    next_due  TEXT NOT NULL,
    last_run  TEXT
);
CREATE INDEX IF NOT EXISTS idx_schedules_due ON schedules(enabled, next_due);
";

/// Open (creating if needed) the daemon database at `path` with WAL + schema.
///
/// # Errors
/// Returns an error if the parent dir can't be created or the DB can't be opened.
pub fn open_db(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    // WAL keeps reads non-blocking against the single writer.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// The durable stores, sharing one connection.
pub struct Stores {
    /// Event log.
    pub events: Arc<dyn EventStore>,
    /// Session registry.
    pub sessions: Arc<dyn SessionStore>,
    /// Conversation history.
    pub messages: Arc<dyn MessageStore>,
    /// Cross-session agent memory (hermes-style persistent recall).
    pub memory: Arc<dyn Memory>,
    /// Scheduled/proactive task definitions.
    pub schedules: Arc<dyn ScheduleStore>,
}

/// Open the durable event + session + message stores at `path`, sharing one
/// connection.
///
/// A single serialized writer is plenty for a single-tenant daemon and avoids
/// SQLite write contention.
///
/// # Errors
/// Returns an error if the database cannot be opened/initialized.
pub fn open_stores(path: &Path) -> anyhow::Result<Stores> {
    let conn = Arc::new(Mutex::new(open_db(path)?));
    Ok(Stores {
        events: Arc::new(SqliteEventLog { conn: Arc::clone(&conn) }),
        sessions: Arc::new(SqliteSessionStore { conn: Arc::clone(&conn) }),
        messages: Arc::new(SqliteMessageStore { conn: Arc::clone(&conn) }),
        memory: Arc::new(SqliteMemory { conn: Arc::clone(&conn) }),
        schedules: Arc::new(SqliteScheduleStore { conn }),
    })
}

fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(PoisonError::into_inner)
}

fn rfc3339_to_utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Durable [`EventStore`] backed by the `events` table.
pub struct SqliteEventLog {
    conn: Arc<Mutex<Connection>>,
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<DaemonEvent> {
    let seq: i64 = row.get(0)?;
    let session_id: String = row.get(1)?;
    let ts: String = row.get(2)?;
    let kind: String = row.get(3)?;
    let kind: EventKind = serde_json::from_str(&kind).map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(DaemonEvent {
        seq: u64::try_from(seq).unwrap_or(0),
        session_id,
        ts: rfc3339_to_utc(&ts),
        kind,
    })
}

#[async_trait]
impl EventStore for SqliteEventLog {
    async fn append(&self, session_id: &str, kind: EventKind) -> anyhow::Result<DaemonEvent> {
        let ts = Utc::now();
        let kind_json = serde_json::to_string(&kind)?;
        let seq = {
            let guard = lock(&self.conn);
            guard.execute(
                "INSERT INTO events (session_id, ts, kind) VALUES (?1, ?2, ?3)",
                params![session_id, ts.to_rfc3339(), kind_json],
            )?;
            u64::try_from(guard.last_insert_rowid()).unwrap_or(0)
        };
        Ok(DaemonEvent {
            seq,
            session_id: session_id.to_owned(),
            ts,
            kind,
        })
    }

    async fn since(&self, cursor: Seq, session_id: Option<&str>, limit: usize) -> anyhow::Result<Vec<DaemonEvent>> {
        let cursor = i64::try_from(cursor).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let guard = lock(&self.conn);
        let mut out = Vec::new();
        if let Some(sid) = session_id {
            let mut stmt = guard.prepare("SELECT seq, session_id, ts, kind FROM events WHERE seq > ?1 AND session_id = ?2 ORDER BY seq LIMIT ?3")?;
            for row in stmt.query_map(params![cursor, sid, limit], row_to_event)? {
                out.push(row?);
            }
        } else {
            let mut stmt = guard.prepare("SELECT seq, session_id, ts, kind FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
            for row in stmt.query_map(params![cursor, limit], row_to_event)? {
                out.push(row?);
            }
        }
        Ok(out)
    }

    async fn latest_seq(&self) -> anyhow::Result<Seq> {
        let guard = lock(&self.conn);
        let seq: i64 = guard.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?;
        Ok(u64::try_from(seq).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Durable [`SessionStore`] backed by the `sessions` table.
pub struct SqliteSessionStore {
    conn: Arc<Mutex<Connection>>,
}

fn status_to_str(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Active => "active",
        SessionStatus::Idle => "idle",
        SessionStatus::Completed => "completed",
    }
}

fn status_from_str(s: &str) -> SessionStatus {
    match s {
        "active" => SessionStatus::Active,
        "completed" => SessionStatus::Completed,
        _ => SessionStatus::Idle,
    }
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let id: String = row.get(0)?;
    let title: Option<String> = row.get(1)?;
    let created_at: String = row.get(2)?;
    let updated_at: String = row.get(3)?;
    let status: String = row.get(4)?;
    Ok(Session {
        id,
        title,
        created_at: rfc3339_to_utc(&created_at),
        updated_at: rfc3339_to_utc(&updated_at),
        status: status_from_str(&status),
    })
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create(&self, id: Option<String>, title: Option<String>) -> anyhow::Result<Session> {
        let now = Utc::now();
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let guard = lock(&self.conn);
        if let Ok(existing) = guard.query_row(
            "SELECT id, title, created_at, updated_at, status FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        ) {
            return Ok(existing);
        }
        let session = Session {
            id,
            title,
            created_at: now,
            updated_at: now,
            status: SessionStatus::Idle,
        };
        guard.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session.id, session.title, now.to_rfc3339(), now.to_rfc3339(), status_to_str(session.status)],
        )?;
        Ok(session)
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<Session>> {
        let guard = lock(&self.conn);
        let found = guard
            .query_row(
                "SELECT id, title, created_at, updated_at, status FROM sessions WHERE id = ?1",
                params![id],
                row_to_session,
            )
            .ok();
        Ok(found)
    }

    async fn list(&self) -> anyhow::Result<Vec<Session>> {
        let guard = lock(&self.conn);
        let mut stmt = guard.prepare("SELECT id, title, created_at, updated_at, status FROM sessions ORDER BY updated_at DESC")?;
        let mut out = Vec::new();
        for row in stmt.query_map([], row_to_session)? {
            out.push(row?);
        }
        Ok(out)
    }

    async fn touch(&self, id: &str) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute("UPDATE sessions SET updated_at = ?1 WHERE id = ?2", params![Utc::now().to_rfc3339(), id])?;
        Ok(())
    }

    async fn set_status(&self, id: &str, status: SessionStatus) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status_to_str(status), Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    async fn set_title_if_unset(&self, id: &str, title: &str) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3 AND (title IS NULL OR title = '')",
            params![title, Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Conversation messages
// ---------------------------------------------------------------------------

/// Durable [`MessageStore`] backed by the `session_messages` table.
pub struct SqliteMessageStore {
    conn: Arc<Mutex<Connection>>,
}

#[async_trait]
impl MessageStore for SqliteMessageStore {
    async fn append(&self, session_id: &str, role: &str, content: &str) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute(
            "INSERT INTO session_messages (session_id, role, content) VALUES (?1, ?2, ?3)",
            params![session_id, role, content],
        )?;
        Ok(())
    }

    async fn load(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<StoredMessage>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let guard = lock(&self.conn);
        let mut stmt = guard.prepare("SELECT role, content FROM session_messages WHERE session_id = ?1 ORDER BY id LIMIT ?2")?;
        let mut out = Vec::new();
        for row in stmt.query_map(params![session_id, limit], |r| {
            Ok(StoredMessage {
                role: r.get(0)?,
                content: r.get(1)?,
            })
        })? {
            out.push(row?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Memory (hermes-style cross-session recall)
// ---------------------------------------------------------------------------

/// Durable [`Memory`] backed by the `memories` table.
///
/// Implements the engine's synchronous `Memory` trait so an always-on agent's
/// recall survives restarts. Recall mirrors `InMemoryMemory`: keyword scoring
/// (fraction of query words found in the content), highest-scoring first.
pub struct SqliteMemory {
    conn: Arc<Mutex<Connection>>,
}

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
    let id: String = row.get(0)?;
    let content: String = row.get(1)?;
    let memory_type: String = row.get(2)?;
    let relevance: f64 = row.get(3)?;
    let metadata: String = row.get(4)?;
    let created_at: String = row.get(5)?;
    let last_accessed: String = row.get(6)?;
    let memory_type: MemoryType =
        serde_json::from_str(&memory_type).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;
    let metadata: HashMap<String, String> =
        serde_json::from_str(&metadata).map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;
    #[allow(clippy::cast_possible_truncation)]
    Ok(MemoryEntry {
        id,
        content,
        memory_type,
        relevance: relevance as f32,
        metadata,
        created_at: rfc3339_to_utc(&created_at),
        last_accessed: rfc3339_to_utc(&last_accessed),
    })
}

impl Memory for SqliteMemory {
    fn store(&self, entry: MemoryEntry) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute(
            "INSERT OR REPLACE INTO memories (id, content, memory_type, relevance, metadata, created_at, last_accessed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.content,
                serde_json::to_string(&entry.memory_type)?,
                f64::from(entry.relevance),
                serde_json::to_string(&entry.metadata)?,
                entry.created_at.to_rfc3339(),
                entry.last_accessed.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        let query_words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if query_words.is_empty() {
            return Ok(Vec::new());
        }
        let guard = lock(&self.conn);
        let mut stmt = guard.prepare("SELECT id, content, memory_type, relevance, metadata, created_at, last_accessed FROM memories")?;
        let mut scored: Vec<(f32, MemoryEntry)> = Vec::new();
        for row in stmt.query_map([], row_to_memory)? {
            let entry = row?;
            let content_lower = entry.content.to_lowercase();
            let matching = query_words.iter().filter(|w| content_lower.contains(w.as_str())).count();
            if matching > 0 {
                #[allow(clippy::cast_precision_loss)]
                let score = matching as f32 / query_words.len() as f32;
                let mut recalled = entry;
                recalled.relevance = score;
                scored.push((score, recalled));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, entry)| entry).collect())
    }

    fn forget(&self, id: &str) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schedules (proactive tasks)
// ---------------------------------------------------------------------------

/// Durable [`ScheduleStore`] backed by the `schedules` table.
pub struct SqliteScheduleStore {
    conn: Arc<Mutex<Connection>>,
}

fn row_to_schedule(row: &rusqlite::Row) -> rusqlite::Result<Schedule> {
    let id: String = row.get(0)?;
    let prompt: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let enabled: i64 = row.get(3)?;
    let next_due: String = row.get(4)?;
    let last_run: Option<String> = row.get(5)?;
    let kind: ScheduleKind = serde_json::from_str(&kind).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(Schedule {
        id,
        prompt,
        kind,
        enabled: enabled != 0,
        next_due: rfc3339_to_utc(&next_due),
        last_run: last_run.as_deref().map(rfc3339_to_utc),
    })
}

#[async_trait]
impl ScheduleStore for SqliteScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> anyhow::Result<()> {
        let kind = serde_json::to_string(&schedule.kind)?;
        let guard = lock(&self.conn);
        guard.execute(
            "INSERT OR REPLACE INTO schedules (id, prompt, kind, enabled, next_due, last_run) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                schedule.id,
                schedule.prompt,
                kind,
                i64::from(schedule.enabled),
                schedule.next_due.to_rfc3339(),
                schedule.last_run.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    async fn list(&self) -> anyhow::Result<Vec<Schedule>> {
        let guard = lock(&self.conn);
        let mut stmt = guard.prepare("SELECT id, prompt, kind, enabled, next_due, last_run FROM schedules ORDER BY next_due")?;
        let mut out = Vec::new();
        for row in stmt.query_map([], row_to_schedule)? {
            out.push(row?);
        }
        Ok(out)
    }

    async fn due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Schedule>> {
        // SQL narrows to enabled rows; the precise due check uses `DateTime`
        // comparison (via `is_due`) to avoid rfc3339 fractional-second string
        // edge cases.
        let guard = lock(&self.conn);
        let mut stmt = guard.prepare("SELECT id, prompt, kind, enabled, next_due, last_run FROM schedules WHERE enabled = 1 ORDER BY next_due")?;
        let mut out = Vec::new();
        for row in stmt.query_map([], row_to_schedule)? {
            let schedule = row?;
            if schedule.is_due(now) {
                out.push(schedule);
            }
        }
        Ok(out)
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let guard = lock(&self.conn);
        guard.execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn db_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("daemon.db")
    }

    #[tokio::test]
    async fn events_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let events = open_stores(&path).unwrap().events;
            events.append("s1", EventKind::TokenDelta { text: "a".into() }).await.unwrap();
            events.append("s1", EventKind::TokenDelta { text: "b".into() }).await.unwrap();
            assert_eq!(events.latest_seq().await.unwrap(), 2);
        }
        // Reopen → durable.
        let events = open_stores(&path).unwrap().events;
        assert_eq!(events.latest_seq().await.unwrap(), 2, "events survived reopen");
        let tail = events.since(1, None, 100).await.unwrap();
        assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
    }

    #[tokio::test]
    async fn events_since_filters_by_session_and_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let events = open_stores(&db_path(&dir)).unwrap().events;
        events.append("s1", EventKind::TokenDelta { text: "a".into() }).await.unwrap(); // 1
        events.append("s2", EventKind::TokenDelta { text: "b".into() }).await.unwrap(); // 2
        events.append("s1", EventKind::TokenDelta { text: "c".into() }).await.unwrap(); // 3
        let only_s1 = events.since(0, Some("s1"), 100).await.unwrap();
        assert_eq!(only_s1.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
        let after_2 = events.since(2, None, 100).await.unwrap();
        assert_eq!(after_2.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3]);
    }

    #[tokio::test]
    async fn messages_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let messages = open_stores(&path).unwrap().messages;
            messages.append("s1", "user", "hi").await.unwrap();
            messages.append("s1", "assistant", "hello there").await.unwrap();
        }
        let messages = open_stores(&path).unwrap().messages;
        let history = messages.load("s1", 100).await.unwrap();
        assert_eq!(history.len(), 2, "messages survived reopen");
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].content, "hello there");
    }

    #[tokio::test]
    async fn memory_persists_recalls_by_keyword_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let forget_id;
        {
            let memory = open_stores(&path).unwrap().memory;
            memory.store(MemoryEntry::new("The user prefers Rust over Go", MemoryType::User)).unwrap();
            memory
                .store(MemoryEntry::new("Deploys go through GitHub Actions, never locally", MemoryType::Project).with_metadata("k", "v"))
                .unwrap();
            let throwaway = MemoryEntry::new("ephemeral note", MemoryType::ShortTerm);
            forget_id = throwaway.id.clone();
            memory.store(throwaway).unwrap();
        }
        // Reopen → durable; recall scores by matching query words.
        let memory = open_stores(&path).unwrap().memory;
        let hits = memory.recall("rust preferences", 5).unwrap();
        assert!(!hits.is_empty(), "keyword recall returns the matching memory");
        assert!(hits[0].content.contains("Rust"), "best match first: {:?}", hits[0].content);
        assert_eq!(hits[0].memory_type, MemoryType::User);

        // Metadata round-trips.
        let deploy = memory.recall("deploys github", 5).unwrap();
        assert_eq!(deploy[0].metadata.get("k").map(String::as_str), Some("v"));

        // Empty query recalls nothing; forget removes by id.
        assert!(memory.recall("", 5).unwrap().is_empty());
        memory.forget(&forget_id).unwrap();
        assert!(memory.recall("ephemeral", 5).unwrap().is_empty(), "forgotten memory is gone");
    }

    #[tokio::test]
    async fn schedules_persist_across_reopen_and_due_filters() {
        use crate::schedule::{Schedule, ScheduleKind};
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z").unwrap().with_timezone(&Utc);
        {
            let schedules = open_stores(&path).unwrap().schedules;
            let mut due_one = Schedule::new("a", "morning brief", ScheduleKind::DailyAt { hour: 6, minute: 0 }, now);
            due_one.next_due = chrono::DateTime::parse_from_rfc3339("2026-06-23T11:59:00Z").unwrap().with_timezone(&Utc);
            schedules.upsert(due_one).await.unwrap();
            schedules
                .upsert(Schedule::new("b", "later", ScheduleKind::EveryNSeconds { secs: 3600 }, now))
                .await
                .unwrap();
        }
        // Reopen → durable; round-trips the kind + timestamps.
        let schedules = open_stores(&path).unwrap().schedules;
        assert_eq!(schedules.list().await.unwrap().len(), 2, "schedules survived reopen");
        let due = schedules.due(now).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "a");
        assert_eq!(due[0].kind, ScheduleKind::DailyAt { hour: 6, minute: 0 });

        schedules.delete("a").await.unwrap();
        assert!(schedules.due(now).await.unwrap().is_empty());
        assert_eq!(schedules.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sessions_persist_and_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let created_id;
        {
            let sessions = open_stores(&path).unwrap().sessions;
            let s = sessions.create(Some("s1".into()), Some("hack".into())).await.unwrap();
            created_id = s.id.clone();
            assert_eq!(s.status, SessionStatus::Idle);
            // Idempotent create.
            let again = sessions.create(Some("s1".into()), Some("ignored".into())).await.unwrap();
            assert_eq!(again.title.as_deref(), Some("hack"));
            sessions.set_status("s1", SessionStatus::Active).await.unwrap();
        }
        let sessions = open_stores(&path).unwrap().sessions;
        let got = sessions.get(&created_id).await.unwrap().expect("session survived reopen");
        assert_eq!(got.status, SessionStatus::Active);
        assert_eq!(sessions.list().await.unwrap().len(), 1);
    }
}
