//! Durable [`StorageAdapter`] for the operator local flavor (EPIC th-c89c2a,
//! convergence 1/4, th-558df1).
//!
//! The operator's local flavor shipped in-memory only — an always-on,
//! self-hosted daemon lost every conversation/session on restart, and the only
//! durable backend was Postgres (the cloud flavor). This adapter fills the gap
//! **without external services**: it mirrors the in-memory adapter's layout
//! (HashMaps for fast reads) and **writes through to a local sqlite file** on
//! every mutation, **loading it back on open**. So `th daemon operator` persists
//! conversations / participants / messages / sessions across restarts.
//!
//! v1 scope: the OLTP slices are durable. Checkpoints + knowledge delegate to
//! the engine's in-memory stores (durable checkpoint/KB persistence is a
//! follow-up); the daemon re-seeds the KB at startup, and resume-from-checkpoint
//! is in-process. The adapter is wired into `serve_local_flavor` via the
//! operator's `LocalServerBuilder::storage(...)` seam (operator commit c74b417).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::Connection;
use serde::Serialize;

use smooth_operator::{CheckpointStore, InMemoryKnowledge, KnowledgeBase, Memory, MemoryCheckpointStore, MemoryEntry};
use smooth_operator_svc::access_control::{AccessContext, AclKnowledgeStore};
use smooth_operator_svc::adapter::{ConversationUpdate, MessagePage, MessageQuery, SessionUpdate, StorageAdapter};
use smooth_operator_svc::domain::{Conversation, Message, Participant, Session};

/// The OLTP slices, kept in memory for reads — identical layout to the operator's
/// in-memory adapter, so the query logic is a faithful mirror.
#[derive(Default)]
struct Tables {
    conversations: HashMap<String, Conversation>,
    participants: HashMap<String, Participant>,
    messages: HashMap<String, Message>,
    /// Append order of message ids per conversation.
    message_order: HashMap<String, Vec<String>>,
    sessions: HashMap<String, Session>,
}

/// Sqlite-backed durable storage adapter (write-through over an in-memory layout).
pub struct SqliteStorageAdapter {
    tables: RwLock<Tables>,
    /// Shared so the durable [`SqliteMemory`] handle ([`Self::memory`]) writes
    /// through the SAME connection — one sqlite file for all the daemon's
    /// durable state (conversations/sessions AND agent memories).
    db: Arc<Mutex<Connection>>,
    /// Durable agent memories (the `remember` tool's backend, hydrated on open).
    /// Kept beside `tables` because the engine's `Memory` trait is its own slice,
    /// not part of `StorageAdapter` at the pinned engine rev — see [`Self::memory`].
    memories: Arc<RwLock<Vec<MemoryEntry>>>,
    checkpoints: Arc<MemoryCheckpointStore>,
    knowledge: AclKnowledgeStore,
}

impl SqliteStorageAdapter {
    /// Open (or create) the durable store at `path` and hydrate the in-memory
    /// tables from it.
    ///
    /// # Errors
    /// Returns an error if the sqlite file can't be opened, the schema can't be
    /// created, or a stored row fails to deserialize.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path).with_context_path(path)?;
        // One key-value table; `rowid` (autoincrement) preserves message append
        // order, `UNIQUE(entity,id)` makes mutations idempotent replaces.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv (
                 rowid  INTEGER PRIMARY KEY AUTOINCREMENT,
                 entity TEXT NOT NULL,
                 id     TEXT NOT NULL,
                 json   TEXT NOT NULL,
                 UNIQUE(entity, id)
             );",
        )?;

        let mut tables = Tables::default();
        let mut memories: Vec<MemoryEntry> = Vec::new();
        {
            let mut stmt = db.prepare("SELECT entity, id, json FROM kv ORDER BY rowid")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            for row in rows {
                let (entity, id, json) = row?;
                match entity.as_str() {
                    "memory" => memories.push(serde_json::from_str(&json)?),
                    "conversation" => {
                        let c: Conversation = serde_json::from_str(&json)?;
                        tables.message_order.entry(c.id.clone()).or_default();
                        tables.conversations.insert(id, c);
                    }
                    "participant" => {
                        tables.participants.insert(id, serde_json::from_str(&json)?);
                    }
                    "message" => {
                        let m: Message = serde_json::from_str(&json)?;
                        if let Some(cid) = &m.conversation_id {
                            tables.message_order.entry(cid.clone()).or_default().push(m.id.clone());
                        }
                        tables.messages.insert(id, m);
                    }
                    "session" => {
                        tables.sessions.insert(id, serde_json::from_str(&json)?);
                    }
                    other => tracing::warn!(entity = other, "unknown durable-storage entity; skipping"),
                }
            }
        }

        Ok(Self {
            tables: RwLock::new(tables),
            db: Arc::new(Mutex::new(db)),
            memories: Arc::new(RwLock::new(memories)),
            checkpoints: Arc::new(MemoryCheckpointStore::new()),
            knowledge: AclKnowledgeStore::new(Arc::new(InMemoryKnowledge::new())),
        })
    }

    /// Write-through one entity as a JSON blob (idempotent replace on `entity,id`).
    fn persist(&self, entity: &str, id: &str, value: &impl Serialize) -> Result<()> {
        persist_kv(&self.db, entity, id, value)
    }

    /// The durable agent-memory backend (the `remember` tool's store, and the
    /// engine's auto-recall source once the engine exposes a seam to inject it).
    ///
    /// Shares this adapter's sqlite connection + the hydrated in-memory vector,
    /// so a `store` is durable immediately and a `recall` sees it after a
    /// restart. Returned as `Arc<dyn Memory>` — exactly the shape both the
    /// `remember` tool and (future) `AgentConfig::with_memory` consume.
    #[must_use]
    pub fn memory(&self) -> Arc<dyn Memory> {
        Arc::new(SqliteMemory {
            db: Arc::clone(&self.db),
            memories: Arc::clone(&self.memories),
        })
    }
}

/// Write-through one entity as a JSON blob (idempotent replace on `entity,id`).
/// Free fn so both the adapter and [`SqliteMemory`] share the exact write path.
fn persist_kv(db: &Mutex<Connection>, entity: &str, id: &str, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string(value)?;
    db.lock().map_err(|e| anyhow!("db lock poisoned: {e}"))?.execute(
        "INSERT OR REPLACE INTO kv(entity, id, json) VALUES(?1, ?2, ?3)",
        rusqlite::params![entity, id, json],
    )?;
    Ok(())
}

/// Durable [`Memory`] backend over the operator storage db (write-through on
/// `store`/`forget`, keyword recall over the in-memory vector). Memories persist
/// in the shared `kv` table under `entity = "memory"`, so they survive a daemon
/// restart — the whole point of th-6d1692. Recall mirrors the engine's
/// `InMemoryMemory` keyword-scoring so injected reminders rank identically.
struct SqliteMemory {
    db: Arc<Mutex<Connection>>,
    memories: Arc<RwLock<Vec<MemoryEntry>>>,
}

impl Memory for SqliteMemory {
    fn store(&self, entry: MemoryEntry) -> Result<()> {
        // Durable FIRST, then the in-memory index — so a write that reaches the
        // caller's "Remembered" ack is already on disk.
        persist_kv(&self.db, "memory", &entry.id, &entry)?;
        self.memories.write().map_err(|e| anyhow!("memory lock poisoned: {e}"))?.push(entry);
        Ok(())
    }

    fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let query_words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if query_words.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.memories.read().map_err(|e| anyhow!("memory lock poisoned: {e}"))?;
        let mut scored: Vec<(f32, MemoryEntry)> = entries
            .iter()
            .filter_map(|entry| {
                let content_lower = entry.content.to_lowercase();
                let matching = query_words.iter().filter(|w| content_lower.contains(w.as_str())).count();
                if matching == 0 {
                    return None;
                }
                #[allow(clippy::cast_precision_loss, reason = "word counts are tiny; f32 is exact here")]
                let score = matching as f32 / query_words.len() as f32;
                let mut recalled = entry.clone();
                recalled.relevance = score;
                Some((score, recalled))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, entry)| entry).collect())
    }

    fn forget(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .map_err(|e| anyhow!("db lock poisoned: {e}"))?
            .execute("DELETE FROM kv WHERE entity = 'memory' AND id = ?1", rusqlite::params![id])?;
        self.memories.write().map_err(|e| anyhow!("memory lock poisoned: {e}"))?.retain(|e| e.id != id);
        Ok(())
    }
}

/// Tiny helper so `open` errors name the path.
trait WithContextPath<T> {
    fn with_context_path(self, path: &Path) -> Result<T>;
}
impl<T> WithContextPath<T> for std::result::Result<T, rusqlite::Error> {
    fn with_context_path(self, path: &Path) -> Result<T> {
        self.map_err(|e| anyhow!("opening durable store {}: {e}", path.display()))
    }
}

#[async_trait]
impl StorageAdapter for SqliteStorageAdapter {
    // ---- conversations ---------------------------------------------------

    async fn create_conversation(&self, conversation: Conversation) -> Result<Conversation> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        if let Some(existing) = t
            .conversations
            .values()
            .find(|c| c.organization_id == conversation.organization_id && c.idempotency_key == conversation.idempotency_key)
        {
            return Ok(existing.clone());
        }
        self.persist("conversation", &conversation.id, &conversation)?;
        t.conversations.insert(conversation.id.clone(), conversation.clone());
        t.message_order.entry(conversation.id.clone()).or_default();
        Ok(conversation)
    }

    async fn get_conversation(&self, id: &str) -> Result<Option<Conversation>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        Ok(t.conversations.get(id).cloned())
    }

    async fn list_conversations_by_org(&self, organization_id: &str) -> Result<Vec<Conversation>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let mut out: Vec<Conversation> = t.conversations.values().filter(|c| c.organization_id == organization_id).cloned().collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        Ok(out)
    }

    async fn update_conversation(&self, id: &str, update: ConversationUpdate) -> Result<Conversation> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let conv = t.conversations.get_mut(id).ok_or_else(|| anyhow!("conversation '{id}' not found"))?;
        if let Some(name) = update.name {
            conv.name = name;
        }
        if update.metadata_json.is_some() {
            conv.metadata_json = update.metadata_json;
        }
        if update.analytics_json.is_some() {
            conv.analytics_json = update.analytics_json;
        }
        conv.updated_at = Utc::now();
        let snapshot = conv.clone();
        self.persist("conversation", id, &snapshot)?;
        Ok(snapshot)
    }

    // ---- participants ----------------------------------------------------

    async fn add_participant(&self, participant: Participant) -> Result<Participant> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        self.persist("participant", &participant.id, &participant)?;
        t.participants.insert(participant.id.clone(), participant.clone());
        Ok(participant)
    }

    async fn get_participant(&self, id: &str) -> Result<Option<Participant>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        Ok(t.participants.get(id).cloned())
    }

    async fn list_participants_by_conversation(&self, conversation_id: &str) -> Result<Vec<Participant>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let mut out: Vec<Participant> = t.participants.values().filter(|p| p.conversation_id == conversation_id).cloned().collect();
        out.sort_by_key(|a| a.created_at);
        Ok(out)
    }

    async fn resolve_participant_by_external_id(&self, conversation_id: &str, external_id: &str) -> Result<Option<Participant>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        Ok(t.participants
            .values()
            .find(|p| p.conversation_id == conversation_id && p.external_id.as_deref() == Some(external_id))
            .cloned())
    }

    // ---- messages --------------------------------------------------------

    async fn append_message(&self, message: Message) -> Result<Message> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        self.persist("message", &message.id, &message)?;
        if let Some(conv_id) = &message.conversation_id {
            t.message_order.entry(conv_id.clone()).or_default().push(message.id.clone());
        }
        t.messages.insert(message.id.clone(), message.clone());
        Ok(message)
    }

    async fn get_message(&self, id: &str) -> Result<Option<Message>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        Ok(t.messages.get(id).cloned())
    }

    async fn list_messages_by_conversation(&self, query: MessageQuery) -> Result<MessagePage> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let order = t.message_order.get(&query.conversation_id).cloned().unwrap_or_default();
        let mut ids: Vec<String> = order;
        if query.descending {
            ids.reverse();
        }
        let start = match &query.cursor {
            Some(cursor) => ids.iter().position(|id| id == cursor).map_or(0, |i| i + 1),
            None => 0,
        };
        let slice: Vec<String> = ids.iter().skip(start).take(query.limit).cloned().collect();
        let next_cursor = if start + slice.len() < ids.len() { slice.last().cloned() } else { None };
        let messages: Vec<Message> = slice.iter().filter_map(|id| t.messages.get(id).cloned()).collect();
        Ok(MessagePage { messages, next_cursor })
    }

    // ---- sessions --------------------------------------------------------

    async fn create_session(&self, session: Session) -> Result<Session> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        self.persist("session", &session.session_id, &session)?;
        t.sessions.insert(session.session_id.clone(), session.clone());
        Ok(session)
    }

    async fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        Ok(t.sessions.get(session_id).cloned())
    }

    async fn update_session(&self, session_id: &str, update: SessionUpdate) -> Result<Session> {
        let mut t = self.tables.write().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let session = t.sessions.get_mut(session_id).ok_or_else(|| anyhow!("session '{session_id}' not found"))?;
        if let Some(status) = update.status {
            session.status = Some(status);
        }
        if let Some(token_count) = update.token_count {
            session.token_count = Some(token_count);
        }
        if let Some(message_count) = update.message_count {
            session.message_count = Some(message_count);
        }
        if update.last_activity_at.is_some() {
            session.last_activity_at = update.last_activity_at;
        }
        if update.ended_at.is_some() {
            session.ended_at = update.ended_at;
        }
        session.updated_at = Some(Utc::now());
        let snapshot = session.clone();
        self.persist("session", session_id, &snapshot)?;
        Ok(snapshot)
    }

    async fn list_sessions_by_conversation(&self, conversation_id: &str) -> Result<Vec<Session>> {
        let t = self.tables.read().map_err(|e| anyhow!("lock poisoned: {e}"))?;
        let mut out: Vec<Session> = t.sessions.values().filter(|s| s.conversation_id == conversation_id).cloned().collect();
        out.sort_by_key(|a| a.created_at);
        Ok(out)
    }

    // ---- engine accessors (in-memory for v1) -----------------------------

    fn checkpoints(&self) -> Arc<dyn CheckpointStore> {
        self.checkpoints.clone()
    }

    fn knowledge(&self) -> Arc<dyn KnowledgeBase> {
        self.knowledge.ingest_handle()
    }

    fn knowledge_for_access(&self, access: &AccessContext) -> Arc<dyn KnowledgeBase> {
        self.knowledge.reader(access.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use smooth_operator_svc::domain::{Platform, SessionStatus};

    fn conv(id: &str) -> Conversation {
        Conversation {
            id: id.into(),
            platform: Platform::Web,
            name: "t".into(),
            organization_id: "org-1".into(),
            idempotency_key: format!("idem-{id}"),
            metadata_json: None,
            analytics_json: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sess(id: &str, conv: &str) -> Session {
        Session {
            session_id: id.into(),
            conversation_id: conv.into(),
            organization_id: "org-1".into(),
            agent_id: "a".into(),
            agent_name: "S".into(),
            user_participant_id: "u".into(),
            agent_participant_id: "ag".into(),
            thread_id: "th".into(),
            status: Some(SessionStatus::Active),
            token_count: Some(0),
            message_count: Some(0),
            metadata: None,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
            ended_at: None,
            last_activity_at: Some(Utc::now()),
        }
    }

    #[tokio::test]
    async fn persists_conversations_and_sessions_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("op.db");
        // Write, then drop (closing the sqlite connection).
        {
            let a = SqliteStorageAdapter::open(&path).unwrap();
            a.create_conversation(conv("conv-1")).await.unwrap();
            a.create_session(sess("sess-1", "conv-1")).await.unwrap();
            assert!(a.get_conversation("conv-1").await.unwrap().is_some());
        }
        // Reopen the same file: the in-memory tables hydrate from sqlite.
        let b = SqliteStorageAdapter::open(&path).unwrap();
        assert!(b.get_conversation("conv-1").await.unwrap().is_some(), "conversation survived restart");
        assert!(b.get_session("sess-1").await.unwrap().is_some(), "session survived restart");
        assert_eq!(b.list_sessions_by_conversation("conv-1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fresh_db_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let a = SqliteStorageAdapter::open(&dir.path().join("empty.db")).unwrap();
        assert!(a.get_conversation("nope").await.unwrap().is_none());
    }

    // ── durable agent memory (th-6d1692) ─────────────────────────────────────

    /// Cross-PROCESS restart proof (run explicitly, not in the default suite):
    ///   PHASE=write  → store a memory to $DB, exit (simulates daemon stop).
    ///   PHASE=recall → reopen $DB in a fresh process, recall, print the hit.
    /// Between the two, `sqlite3 $DB` (a third process) can read the row on disk.
    #[test]
    #[ignore = "manual cross-process durability proof; driven by env vars"]
    fn proof_cross_process_memory_durability() {
        use smooth_operator::MemoryType;
        let path = std::path::PathBuf::from(std::env::var("DB").expect("set DB"));
        match std::env::var("PHASE").as_deref() {
            Ok("write") => {
                let a = SqliteStorageAdapter::open(&path).unwrap();
                a.memory()
                    .store(MemoryEntry::new("always add new shows to the smoo-hub watchlist", MemoryType::User))
                    .unwrap();
                println!("WROTE memory to {}", path.display());
            }
            Ok("recall") => {
                let b = SqliteStorageAdapter::open(&path).unwrap();
                let hits = b.memory().recall("smoo-hub watchlist shows", 5).unwrap();
                assert_eq!(hits.len(), 1, "memory recalled in a fresh process after restart");
                println!("RECALLED after restart: [{:?}] {}", hits[0].memory_type, hits[0].content);
            }
            other => panic!("set PHASE=write|recall (got {other:?})"),
        }
    }

    #[test]
    fn memory_survives_restart_and_is_recallable() {
        use smooth_operator::MemoryType;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("op.db");
        let id;
        // Write a memory, then drop the adapter (closing the sqlite connection).
        {
            let a = SqliteStorageAdapter::open(&path).unwrap();
            let mem = a.memory();
            let entry = MemoryEntry::new("the user deploys via GitHub Actions", MemoryType::Project);
            id = entry.id.clone();
            mem.store(entry).unwrap();
            // Recallable before restart.
            assert_eq!(mem.recall("github actions", 5).unwrap().len(), 1);
        }
        // Reopen the SAME file: memories hydrate from sqlite — the key assertion.
        let b = SqliteStorageAdapter::open(&path).unwrap();
        let mem = b.memory();
        let hits = mem.recall("github actions deploy", 5).unwrap();
        assert_eq!(hits.len(), 1, "memory written before restart is recalled after restart");
        assert_eq!(hits[0].id, id);
        assert!(hits[0].content.contains("GitHub Actions"));
        assert_eq!(hits[0].memory_type, MemoryType::Project);
        assert!(hits[0].relevance > 0.0, "recall scores relevance");
    }

    #[test]
    fn memory_forget_is_durable() {
        use smooth_operator::MemoryType;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("op.db");
        let id;
        {
            let a = SqliteStorageAdapter::open(&path).unwrap();
            let mem = a.memory();
            let entry = MemoryEntry::new("ephemeral fact", MemoryType::ShortTerm);
            id = entry.id.clone();
            mem.store(entry).unwrap();
            mem.forget(&id).unwrap();
            assert!(mem.recall("ephemeral", 5).unwrap().is_empty());
        }
        // The forget persisted: the reopened store has no trace of it.
        let b = SqliteStorageAdapter::open(&path).unwrap();
        assert!(b.memory().recall("ephemeral", 5).unwrap().is_empty(), "forget survives restart");
    }

    #[test]
    fn memory_recall_empty_query_returns_nothing() {
        use smooth_operator::MemoryType;
        let dir = tempfile::tempdir().unwrap();
        let a = SqliteStorageAdapter::open(&dir.path().join("op.db")).unwrap();
        let mem = a.memory();
        mem.store(MemoryEntry::new("some fact", MemoryType::User)).unwrap();
        assert!(mem.recall("   ", 5).unwrap().is_empty(), "blank query recalls nothing");
        assert!(mem.recall("nonexistent term", 5).unwrap().is_empty(), "no keyword match recalls nothing");
    }
}
