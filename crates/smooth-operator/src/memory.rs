use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Trait for pluggable agent memory backends.
pub trait Memory: Send + Sync {
    /// Store a memory entry.
    ///
    /// # Errors
    /// Returns error if the storage backend fails.
    fn store(&self, entry: MemoryEntry) -> anyhow::Result<()>;

    /// Recall memories relevant to a query, returning up to `limit` entries.
    ///
    /// # Errors
    /// Returns error if the retrieval backend fails.
    fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Forget (remove) a memory entry by ID.
    ///
    /// # Errors
    /// Returns error if the deletion backend fails.
    fn forget(&self, id: &str) -> anyhow::Result<()>;
}

/// A single memory entry stored by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub relevance: f32,
    pub metadata: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
}

impl MemoryEntry {
    /// Create a new memory entry with the given content and type.
    pub fn new(content: impl Into<String>, memory_type: MemoryType) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            content: content.into(),
            memory_type,
            relevance: 0.0,
            metadata: HashMap::new(),
            created_at: now,
            last_accessed: now,
        }
    }

    /// Add metadata key-value pair (builder pattern).
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Classification of memory entries.
///
/// The first three variants (`ShortTerm`, `LongTerm`, `Entity`) are
/// scope-based — they describe *when* a memory is valid and don't
/// carry intent.
///
/// The lower four variants (`User`, `Feedback`, `Project`, `Reference`)
/// are intent-based and adapted from the Claude Code v2.1.120 memory
/// subsystem. They tell future calls *how* to use the memory:
///
/// - `User` — durable facts about the user (role, expertise,
///   preferences). Shapes how to address and explain things.
/// - `Feedback` — corrections or confirmations on approach. Highest
///   leverage type — re-reading prevents re-litigating decisions.
/// - `Project` — current state of in-flight work — initiatives,
///   deadlines, who's doing what. Decays fast; verify against current
///   state before acting.
/// - `Reference` — pointers to where information lives outside this
///   project (Linear, Slack channel, dashboard URL, etc.).
///
/// Intent typing matters for *recall*: `Feedback` and `User` entries
/// stay applicable across sessions, while `Project` and `Reference`
/// need a freshness check before being acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    /// Transient, session-scoped memory.
    ShortTerm,
    /// Persisted across sessions.
    LongTerm,
    /// Named entity or concept.
    Entity,
    /// Durable facts about the user — role, expertise, preferences.
    User,
    /// Corrections or validations on approach. Read these especially
    /// carefully; they're meant to prevent repeated drift.
    Feedback,
    /// Current state of in-flight work. Decays quickly; verify against
    /// current state before acting on it.
    Project,
    /// Pointer to where information lives outside this project
    /// (Linear, Slack, Grafana, GitHub, etc.).
    Reference,
}

impl MemoryType {
    /// True if recall sites should append a freshness-check nudge to
    /// any reminder rendered from a memory of this type.
    ///
    /// `Project` and `Reference` memories are time-sensitive — a
    /// claimed function path may have been renamed, an external
    /// dashboard URL may have moved. The other types ride on durable
    /// truths and don't need the same caveat.
    #[must_use]
    pub fn needs_freshness_check(self) -> bool {
        matches!(self, Self::Project | Self::Reference)
    }
}

/// In-memory implementation of the `Memory` trait.
///
/// Uses a `Mutex<Vec<MemoryEntry>>` for thread-safe storage.
/// Recall performs keyword matching: splits the query into words and scores
/// entries by the number of matching words found in the content.
pub struct InMemoryMemory {
    entries: Mutex<Vec<MemoryEntry>>,
}

impl InMemoryMemory {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory for InMemoryMemory {
    fn store(&self, entry: MemoryEntry) -> anyhow::Result<()> {
        let mut entries = self.entries.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        entries.push(entry);
        Ok(())
    }

    fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self.entries.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let query_words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        if query_words.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f32, MemoryEntry)> = entries
            .iter()
            .filter_map(|entry| {
                let content_lower = entry.content.to_lowercase();
                let matching = query_words.iter().filter(|w| content_lower.contains(w.as_str())).count();
                if matching > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    let score = matching as f32 / query_words.len() as f32;
                    let mut recalled = entry.clone();
                    recalled.relevance = score;
                    Some((score, recalled))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored.into_iter().map(|(_, entry)| entry).collect())
    }

    fn forget(&self, id: &str) -> anyhow::Result<()> {
        let mut entries = self.entries.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        entries.retain(|e| e.id != id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_creation_and_serialization() {
        let entry = MemoryEntry::new("test content", MemoryType::ShortTerm).with_metadata("key", "value");

        assert_eq!(entry.content, "test content");
        assert_eq!(entry.memory_type, MemoryType::ShortTerm);
        assert_eq!(entry.metadata.get("key"), Some(&"value".to_string()));
        assert_eq!(entry.relevance, 0.0);

        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: MemoryEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.content, "test content");
        assert_eq!(parsed.memory_type, MemoryType::ShortTerm);
        assert_eq!(parsed.metadata.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn in_memory_store_and_recall() {
        let mem = InMemoryMemory::new();
        mem.store(MemoryEntry::new("rust programming language", MemoryType::LongTerm)).expect("store");
        mem.store(MemoryEntry::new("python data science", MemoryType::LongTerm)).expect("store");

        let results = mem.recall("rust", 10).expect("recall");
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("rust"));
    }

    #[test]
    fn recall_keyword_matching_returns_relevant() {
        let mem = InMemoryMemory::new();
        mem.store(MemoryEntry::new("the quick brown fox jumps over the lazy dog", MemoryType::ShortTerm))
            .expect("store");
        mem.store(MemoryEntry::new("hello world program in rust", MemoryType::ShortTerm))
            .expect("store");
        mem.store(MemoryEntry::new("the fox is quick and clever", MemoryType::ShortTerm))
            .expect("store");

        let results = mem.recall("quick fox", 5).expect("recall");
        assert_eq!(results.len(), 2);
        // The entry with both words should score higher
        assert!(results[0].relevance >= results[1].relevance);
        assert!(results[0].content.contains("quick"));
    }

    #[test]
    fn recall_no_matches_returns_empty() {
        let mem = InMemoryMemory::new();
        mem.store(MemoryEntry::new("rust programming", MemoryType::ShortTerm)).expect("store");

        let results = mem.recall("javascript", 10).expect("recall");
        assert!(results.is_empty());
    }

    #[test]
    fn forget_removes_entry() {
        let mem = InMemoryMemory::new();
        let entry = MemoryEntry::new("to be forgotten", MemoryType::ShortTerm);
        let id = entry.id.clone();
        mem.store(entry).expect("store");

        assert_eq!(mem.recall("forgotten", 10).expect("recall").len(), 1);

        mem.forget(&id).expect("forget");
        assert!(mem.recall("forgotten", 10).expect("recall").is_empty());
    }

    #[test]
    fn memory_type_variants_serialize_correctly() {
        let types = [
            MemoryType::ShortTerm,
            MemoryType::LongTerm,
            MemoryType::Entity,
            MemoryType::User,
            MemoryType::Feedback,
            MemoryType::Project,
            MemoryType::Reference,
        ];
        for mt in &types {
            let json = serde_json::to_string(mt).expect("serialize");
            let parsed: MemoryType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*mt, parsed);
        }

        let json = serde_json::to_string(&MemoryType::ShortTerm).expect("serialize");
        assert!(json.contains("ShortTerm"));
        let json = serde_json::to_string(&MemoryType::Feedback).expect("serialize");
        assert!(json.contains("Feedback"));
        let json = serde_json::to_string(&MemoryType::Reference).expect("serialize");
        assert!(json.contains("Reference"));
    }

    #[test]
    fn freshness_check_only_for_time_sensitive_types() {
        // D6: Project and Reference name external state that decays —
        // the agent must verify before recommending. User and Feedback
        // ride on durable truths and don't need the same caveat. Guards
        // against a refactor that flips a non-decaying type into the
        // freshness-check path (and bloats every recall block) or
        // drops Project/Reference out of it (and loses the recommend-
        // before-verify discipline).
        assert!(MemoryType::Project.needs_freshness_check());
        assert!(MemoryType::Reference.needs_freshness_check());
        assert!(!MemoryType::User.needs_freshness_check());
        assert!(!MemoryType::Feedback.needs_freshness_check());
        assert!(!MemoryType::ShortTerm.needs_freshness_check());
        assert!(!MemoryType::LongTerm.needs_freshness_check());
        assert!(!MemoryType::Entity.needs_freshness_check());
    }
}
