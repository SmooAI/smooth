use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::conversation::Conversation;

/// A checkpoint captures the full state of an agent at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub agent_id: String,
    pub conversation: Conversation,
    pub iteration: u32,
    pub metadata: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
}

impl Checkpoint {
    pub fn new(agent_id: &str, conversation: &Conversation, iteration: u32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            conversation: conversation.clone(),
            iteration,
            metadata: HashMap::new(),
            created_at: Utc::now(),
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Serialize to JSON bytes.
    ///
    /// # Errors
    /// Returns error if serialization fails.
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Deserialize from JSON bytes.
    ///
    /// # Errors
    /// Returns error if deserialization fails.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// Strategy for when to create checkpoints.
#[derive(Debug, Clone, Default)]
pub enum CheckpointStrategy {
    /// After every tool call completion.
    #[default]
    AfterToolCall,
    /// After every N iterations of the agent loop.
    EveryN(u32),
    /// After every message from the LLM.
    AfterLlmResponse,
    /// Never checkpoint (for testing or short tasks).
    Never,
}

impl CheckpointStrategy {
    pub fn should_checkpoint(&self, iteration: u32, event: CheckpointEvent) -> bool {
        match self {
            Self::EveryN(n) => iteration.is_multiple_of(*n),
            Self::AfterToolCall => event == CheckpointEvent::ToolCallComplete,
            Self::AfterLlmResponse => event == CheckpointEvent::LlmResponse,
            Self::Never => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointEvent {
    ToolCallComplete,
    LlmResponse,
    Iteration,
}

/// Trait for checkpoint storage backends.
pub trait CheckpointStore: Send + Sync {
    /// Save a checkpoint.
    ///
    /// # Errors
    /// Returns error if storage fails.
    fn save(&self, checkpoint: &Checkpoint) -> anyhow::Result<()>;

    /// Load the latest checkpoint for an agent.
    ///
    /// # Errors
    /// Returns error if loading fails.
    fn load_latest(&self, agent_id: &str) -> anyhow::Result<Option<Checkpoint>>;

    /// Load a specific checkpoint by ID.
    ///
    /// # Errors
    /// Returns error if loading fails.
    fn load(&self, checkpoint_id: &str) -> anyhow::Result<Option<Checkpoint>>;

    /// List all checkpoints for an agent, newest first.
    ///
    /// # Errors
    /// Returns error if listing fails.
    fn list(&self, agent_id: &str) -> anyhow::Result<Vec<Checkpoint>>;

    /// Delete checkpoints older than the most recent N.
    ///
    /// # Errors
    /// Returns error if deletion fails.
    fn prune(&self, agent_id: &str, keep: usize) -> anyhow::Result<usize>;
}

/// In-memory checkpoint store (for testing and short-lived agents).
pub struct MemoryCheckpointStore {
    checkpoints: Mutex<Vec<Checkpoint>>,
}

impl MemoryCheckpointStore {
    pub fn new() -> Self {
        Self {
            checkpoints: Mutex::new(Vec::new()),
        }
    }
}

impl Default for MemoryCheckpointStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckpointStore for MemoryCheckpointStore {
    fn save(&self, checkpoint: &Checkpoint) -> anyhow::Result<()> {
        let mut store = self.checkpoints.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        store.push(checkpoint.clone());
        Ok(())
    }

    fn load_latest(&self, agent_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        let store = self.checkpoints.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(store.iter().rev().find(|c| c.agent_id == agent_id).cloned())
    }

    fn load(&self, checkpoint_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        let store = self.checkpoints.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(store.iter().find(|c| c.id == checkpoint_id).cloned())
    }

    fn list(&self, agent_id: &str) -> anyhow::Result<Vec<Checkpoint>> {
        let store = self.checkpoints.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut result: Vec<Checkpoint> = store.iter().filter(|c| c.agent_id == agent_id).cloned().collect();
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(result)
    }

    fn prune(&self, agent_id: &str, keep: usize) -> anyhow::Result<usize> {
        let mut store = self.checkpoints.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // Collect indices of this agent's checkpoints, sorted newest first
        let mut agent_indices: Vec<usize> = store.iter().enumerate().filter(|(_, c)| c.agent_id == agent_id).map(|(i, _)| i).collect();

        // Sort by created_at descending (newest first)
        agent_indices.sort_by(|&a, &b| store[b].created_at.cmp(&store[a].created_at));

        // Indices to remove = everything after `keep`
        let mut to_remove: Vec<usize> = agent_indices.into_iter().skip(keep).collect();
        let count = to_remove.len();

        // Sort descending so we remove from the end first (preserves earlier indices)
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for idx in to_remove {
            store.remove(idx);
        }

        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// FileCheckpointStore — JSON files on disk, zero extra deps
// ---------------------------------------------------------------------------

/// File-based checkpoint store. Stores each checkpoint as
/// `{dir}/{agent_id}/{checkpoint_id}.json`.
pub struct FileCheckpointStore {
    dir: PathBuf,
}

impl FileCheckpointStore {
    /// Create a new file-based store rooted at `dir`.
    /// Directories are created lazily on first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn agent_dir(&self, agent_id: &str) -> PathBuf {
        self.dir.join(agent_id)
    }

    fn checkpoint_path(&self, agent_id: &str, checkpoint_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join(format!("{checkpoint_id}.json"))
    }
}

impl CheckpointStore for FileCheckpointStore {
    fn save(&self, checkpoint: &Checkpoint) -> anyhow::Result<()> {
        let dir = self.agent_dir(&checkpoint.agent_id);
        std::fs::create_dir_all(&dir)?;
        let path = self.checkpoint_path(&checkpoint.agent_id, &checkpoint.id);
        let bytes = serde_json::to_vec_pretty(checkpoint)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    fn load_latest(&self, agent_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        let list = self.list(agent_id)?;
        Ok(list.into_iter().next())
    }

    fn load(&self, checkpoint_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        // We don't know the agent_id, so scan all agent directories.
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join(format!("{checkpoint_id}.json"));
            if path.exists() {
                let bytes = std::fs::read(&path)?;
                return Ok(Some(serde_json::from_slice(&bytes)?));
            }
        }
        Ok(None)
    }

    fn list(&self, agent_id: &str) -> anyhow::Result<Vec<Checkpoint>> {
        let dir = self.agent_dir(agent_id);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let bytes = std::fs::read(&path)?;
                checkpoints.push(serde_json::from_slice(&bytes)?);
            }
        }
        // Newest first
        checkpoints.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(checkpoints)
    }

    fn prune(&self, agent_id: &str, keep: usize) -> anyhow::Result<usize> {
        let list = self.list(agent_id)?;
        let to_remove = list.into_iter().skip(keep).collect::<Vec<_>>();
        let count = to_remove.len();
        for cp in &to_remove {
            let path = self.checkpoint_path(agent_id, &cp.id);
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// SqliteCheckpointStore — persistent SQLite-backed store
// ---------------------------------------------------------------------------

#[cfg(feature = "sqlite")]
use std::sync::Arc;

/// SQLite-backed checkpoint store with WAL mode.
#[cfg(feature = "sqlite")]
pub struct SqliteCheckpointStore {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

#[cfg(feature = "sqlite")]
impl SqliteCheckpointStore {
    const SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS checkpoints (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            iteration INTEGER NOT NULL,
            conversation BLOB NOT NULL,
            metadata TEXT DEFAULT '{}',
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_checkpoints_agent
            ON checkpoints(agent_id, created_at DESC);
    ";

    /// Open (or create) a SQLite checkpoint store at `path`.
    ///
    /// # Errors
    /// Returns error if the database cannot be opened or the schema cannot be created.
    pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(Self::SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Create an in-memory SQLite checkpoint store (for testing).
    ///
    /// # Errors
    /// Returns error if the in-memory database cannot be created.
    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(Self::SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

#[cfg(feature = "sqlite")]
impl CheckpointStore for SqliteCheckpointStore {
    fn save(&self, checkpoint: &Checkpoint) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let conversation_blob = serde_json::to_vec(&checkpoint.conversation)?;
        let metadata_json = serde_json::to_string(&checkpoint.metadata)?;
        let created_at = checkpoint.created_at.timestamp();

        conn.execute(
            "INSERT OR REPLACE INTO checkpoints (id, agent_id, iteration, conversation, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                checkpoint.id,
                checkpoint.agent_id,
                checkpoint.iteration,
                conversation_blob,
                metadata_json,
                created_at
            ],
        )?;
        Ok(())
    }

    fn load_latest(&self, agent_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, iteration, conversation, metadata, created_at
             FROM checkpoints WHERE agent_id = ?1
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![agent_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_checkpoint(row)?)),
            None => Ok(None),
        }
    }

    fn load(&self, checkpoint_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, iteration, conversation, metadata, created_at
             FROM checkpoints WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![checkpoint_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_checkpoint(row)?)),
            None => Ok(None),
        }
    }

    fn list(&self, agent_id: &str) -> anyhow::Result<Vec<Checkpoint>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, iteration, conversation, metadata, created_at
             FROM checkpoints WHERE agent_id = ?1
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![agent_id], |row| {
            row_to_checkpoint(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, e.into()))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    fn prune(&self, agent_id: &str, keep: usize) -> anyhow::Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;

        // Get IDs to keep (newest N)
        let keep_i64 = i64::try_from(keep).unwrap_or(i64::MAX);
        let deleted = conn.execute(
            "DELETE FROM checkpoints WHERE agent_id = ?1 AND id NOT IN (
                SELECT id FROM checkpoints WHERE agent_id = ?1
                ORDER BY created_at DESC LIMIT ?2
            )",
            rusqlite::params![agent_id, keep_i64],
        )?;
        Ok(deleted)
    }
}

/// Convert a SQLite row into a `Checkpoint`.
#[cfg(feature = "sqlite")]
fn row_to_checkpoint(row: &rusqlite::Row<'_>) -> anyhow::Result<Checkpoint> {
    let id: String = row.get(0)?;
    let agent_id: String = row.get(1)?;
    let iteration: u32 = row.get(2)?;
    let conversation_blob: Vec<u8> = row.get(3)?;
    let metadata_json: String = row.get(4)?;
    let created_at_ts: i64 = row.get(5)?;

    let conversation: crate::conversation::Conversation = serde_json::from_slice(&conversation_blob)?;
    let metadata: HashMap<String, String> = serde_json::from_str(&metadata_json)?;
    let created_at = DateTime::from_timestamp(created_at_ts, 0).ok_or_else(|| anyhow::anyhow!("invalid timestamp: {created_at_ts}"))?;

    Ok(Checkpoint {
        id,
        agent_id,
        conversation,
        iteration,
        metadata,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Conversation;

    fn test_checkpoint(agent_id: &str, iteration: u32) -> Checkpoint {
        let conv = Conversation::new(100_000).with_system_prompt("test");
        Checkpoint::new(agent_id, &conv, iteration)
    }

    #[test]
    fn checkpoint_creation() {
        let cp = test_checkpoint("agent-1", 5);
        assert_eq!(cp.agent_id, "agent-1");
        assert_eq!(cp.iteration, 5);
        assert!(!cp.id.is_empty());
    }

    #[test]
    fn checkpoint_with_metadata() {
        let cp = test_checkpoint("agent-1", 1)
            .with_metadata("phase", "execute")
            .with_metadata("bead_id", "smooth-abc");
        assert_eq!(cp.metadata.get("phase").map(String::as_str), Some("execute"));
        assert_eq!(cp.metadata.get("bead_id").map(String::as_str), Some("smooth-abc"));
    }

    #[test]
    fn checkpoint_serialization() {
        let cp = test_checkpoint("agent-1", 3);
        let bytes = cp.to_bytes().expect("serialize");
        let restored = Checkpoint::from_bytes(&bytes).expect("deserialize");
        assert_eq!(restored.agent_id, "agent-1");
        assert_eq!(restored.iteration, 3);
    }

    #[test]
    fn memory_store_save_and_load() {
        let store = MemoryCheckpointStore::new();
        let cp = test_checkpoint("agent-1", 1);
        store.save(&cp).expect("save");

        let latest = store.load_latest("agent-1").expect("load").expect("should exist");
        assert_eq!(latest.agent_id, "agent-1");
    }

    #[test]
    fn memory_store_load_by_id() {
        let store = MemoryCheckpointStore::new();
        let cp = test_checkpoint("agent-1", 1);
        let id = cp.id.clone();
        store.save(&cp).expect("save");

        let loaded = store.load(&id).expect("load").expect("should exist");
        assert_eq!(loaded.id, id);
    }

    #[test]
    fn memory_store_load_nonexistent() {
        let store = MemoryCheckpointStore::new();
        assert!(store.load_latest("nonexistent").expect("load").is_none());
        assert!(store.load("bad-id").expect("load").is_none());
    }

    #[test]
    fn memory_store_list_ordered() {
        let store = MemoryCheckpointStore::new();
        for i in 0..5 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
        }
        store.save(&test_checkpoint("agent-2", 0)).expect("save");

        let list = store.list("agent-1").expect("list");
        assert_eq!(list.len(), 5);
    }

    #[test]
    fn memory_store_prune() {
        let store = MemoryCheckpointStore::new();
        for i in 0..10 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
        }

        let removed = store.prune("agent-1", 3).expect("prune");
        assert_eq!(removed, 7);

        let remaining = store.list("agent-1").expect("list");
        assert_eq!(remaining.len(), 3);
    }

    #[test]
    fn memory_store_prune_different_agents() {
        let store = MemoryCheckpointStore::new();
        for i in 0..5 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
            store.save(&test_checkpoint("agent-2", i)).expect("save");
        }

        store.prune("agent-1", 2).expect("prune");
        assert_eq!(store.list("agent-1").expect("list").len(), 2);
        assert_eq!(store.list("agent-2").expect("list").len(), 5); // untouched
    }

    #[test]
    fn strategy_every_n() {
        let strategy = CheckpointStrategy::EveryN(3);
        assert!(!strategy.should_checkpoint(1, CheckpointEvent::Iteration));
        assert!(!strategy.should_checkpoint(2, CheckpointEvent::Iteration));
        assert!(strategy.should_checkpoint(3, CheckpointEvent::Iteration));
        assert!(strategy.should_checkpoint(6, CheckpointEvent::Iteration));
    }

    #[test]
    fn strategy_after_tool_call() {
        let strategy = CheckpointStrategy::AfterToolCall;
        assert!(strategy.should_checkpoint(1, CheckpointEvent::ToolCallComplete));
        assert!(!strategy.should_checkpoint(1, CheckpointEvent::LlmResponse));
    }

    #[test]
    fn strategy_never() {
        let strategy = CheckpointStrategy::Never;
        assert!(!strategy.should_checkpoint(1, CheckpointEvent::ToolCallComplete));
        assert!(!strategy.should_checkpoint(1, CheckpointEvent::LlmResponse));
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::conversation::Conversation;

    fn test_checkpoint(agent_id: &str, iteration: u32) -> Checkpoint {
        let conv = Conversation::new(100_000).with_system_prompt("test");
        Checkpoint::new(agent_id, &conv, iteration)
    }

    #[test]
    fn file_store_save_creates_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store = FileCheckpointStore::new(dir.path());
        let cp = test_checkpoint("agent-1", 1);
        let id = cp.id.clone();
        store.save(&cp).expect("save");

        let path = dir.path().join("agent-1").join(format!("{id}.json"));
        assert!(path.exists(), "checkpoint file should exist on disk");
    }

    #[test]
    fn file_store_load_roundtrip() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store = FileCheckpointStore::new(dir.path());
        let cp = test_checkpoint("agent-1", 42);
        let id = cp.id.clone();
        store.save(&cp).expect("save");

        let loaded = store.load(&id).expect("load").expect("should exist");
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.agent_id, "agent-1");
        assert_eq!(loaded.iteration, 42);
    }

    #[test]
    fn file_store_list_scans_directory() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store = FileCheckpointStore::new(dir.path());
        for i in 0..5 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
        }
        store.save(&test_checkpoint("agent-2", 0)).expect("save");

        let list = store.list("agent-1").expect("list");
        assert_eq!(list.len(), 5);
    }

    #[test]
    fn file_store_prune_deletes_old() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store = FileCheckpointStore::new(dir.path());
        for i in 0..10 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
        }

        let removed = store.prune("agent-1", 3).expect("prune");
        assert_eq!(removed, 7);
        assert_eq!(store.list("agent-1").expect("list").len(), 3);
    }

    #[test]
    fn file_store_nested_agent_dirs() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store = FileCheckpointStore::new(dir.path());
        store.save(&test_checkpoint("agent-a", 1)).expect("save");
        store.save(&test_checkpoint("agent-b", 1)).expect("save");

        assert!(dir.path().join("agent-a").is_dir());
        assert!(dir.path().join("agent-b").is_dir());
    }

    #[test]
    fn file_store_missing_dir_created() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let nested = dir.path().join("deep").join("nested").join("store");
        let store = FileCheckpointStore::new(&nested);
        store.save(&test_checkpoint("agent-1", 1)).expect("save");

        assert!(nested.join("agent-1").is_dir());
        assert_eq!(store.list("agent-1").expect("list").len(), 1);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod sqlite_tests {
    use super::*;
    use crate::conversation::Conversation;

    fn test_checkpoint(agent_id: &str, iteration: u32) -> Checkpoint {
        let conv = Conversation::new(100_000).with_system_prompt("test");
        Checkpoint::new(agent_id, &conv, iteration)
    }

    #[test]
    fn sqlite_open_creates_schema() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let db_path = dir.path().join("test.db");
        let store = SqliteCheckpointStore::open(&db_path).expect("open");

        // Verify schema exists by saving a checkpoint
        let cp = test_checkpoint("agent-1", 1);
        store.save(&cp).expect("save should work after schema creation");
    }

    #[test]
    fn sqlite_save_load_latest_roundtrip() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        let cp = test_checkpoint("agent-1", 5);
        store.save(&cp).expect("save");

        let latest = store.load_latest("agent-1").expect("load").expect("should exist");
        assert_eq!(latest.agent_id, "agent-1");
        assert_eq!(latest.iteration, 5);
    }

    #[test]
    fn sqlite_load_by_id() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        let cp = test_checkpoint("agent-1", 1);
        let id = cp.id.clone();
        store.save(&cp).expect("save");

        let loaded = store.load(&id).expect("load").expect("should exist");
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.agent_id, "agent-1");
    }

    #[test]
    fn sqlite_list_newest_first() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        for i in 0..5 {
            let mut cp = test_checkpoint("agent-1", i);
            // Offset created_at so ordering is deterministic
            cp.created_at = Utc::now() + chrono::Duration::seconds(i64::from(i));
            store.save(&cp).expect("save");
        }
        store.save(&test_checkpoint("agent-2", 0)).expect("save");

        let list = store.list("agent-1").expect("list");
        assert_eq!(list.len(), 5);
        // Newest (iteration 4) should be first
        assert!(list[0].created_at >= list[1].created_at);
    }

    #[test]
    fn sqlite_prune_keeps_n() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        for i in 0..10 {
            let mut cp = test_checkpoint("agent-1", i);
            cp.created_at = Utc::now() + chrono::Duration::seconds(i64::from(i));
            store.save(&cp).expect("save");
        }

        let removed = store.prune("agent-1", 3).expect("prune");
        assert_eq!(removed, 7);
        assert_eq!(store.list("agent-1").expect("list").len(), 3);
    }

    #[test]
    fn sqlite_prune_isolates_agents() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        for i in 0..5 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
            store.save(&test_checkpoint("agent-2", i)).expect("save");
        }

        store.prune("agent-1", 2).expect("prune");
        assert_eq!(store.list("agent-1").expect("list").len(), 2);
        assert_eq!(store.list("agent-2").expect("list").len(), 5);
    }

    #[test]
    fn sqlite_nonexistent_returns_none() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        assert!(store.load_latest("nonexistent").expect("load").is_none());
        assert!(store.load("bad-id").expect("load").is_none());
    }

    #[test]
    fn sqlite_in_memory_works() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        let cp = test_checkpoint("mem-agent", 7);
        store.save(&cp).expect("save");
        let loaded = store.load_latest("mem-agent").expect("load").expect("exists");
        assert_eq!(loaded.iteration, 7);
    }

    #[test]
    fn sqlite_concurrent_saves() {
        let store = SqliteCheckpointStore::in_memory().expect("in_memory");
        // Simulate concurrent saves (single-threaded but exercises locking)
        for i in 0..50 {
            store.save(&test_checkpoint("agent-1", i)).expect("save");
        }
        assert_eq!(store.list("agent-1").expect("list").len(), 50);
    }
}
