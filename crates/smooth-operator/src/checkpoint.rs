use std::collections::HashMap;
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
