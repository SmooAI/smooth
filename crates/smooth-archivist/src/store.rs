use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use smooth_scribe::{LogEntry, LogLevel};

use crate::ingest::{IngestBatch, IngestResult};

/// Query parameters for cross-VM log search.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveQuery {
    pub operator_id: Option<String>,
    pub vm: Option<String>,
    pub min_level: Option<LogLevel>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for ArchiveQuery {
    fn default() -> Self {
        Self {
            operator_id: None,
            vm: None,
            min_level: None,
            limit: 100,
        }
    }
}

fn default_limit() -> usize {
    100
}

/// Aggregate statistics across all VMs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveStats {
    pub total_entries: usize,
    pub by_vm: HashMap<String, usize>,
    pub by_level: HashMap<String, usize>,
}

/// Trait for the central archive storage backend.
pub trait ArchiveStore: Send + Sync {
    /// Ingest a batch of log entries from a Scribe.
    fn ingest(&self, batch: IngestBatch) -> IngestResult;

    /// Query entries across all VMs.
    fn query(&self, query: &ArchiveQuery) -> Vec<LogEntry>;

    /// Query entries by operator ID.
    fn query_by_operator(&self, operator_id: &str) -> Vec<LogEntry>;

    /// Get aggregate statistics.
    fn stats(&self) -> ArchiveStats;
}

/// Stored entry with source VM metadata.
#[derive(Debug, Clone)]
struct StoredEntry {
    entry: LogEntry,
    source_vm: String,
}

/// In-memory implementation of `ArchiveStore`.
#[derive(Debug, Default)]
pub struct MemoryArchiveStore {
    entries: Mutex<Vec<StoredEntry>>,
}

impl MemoryArchiveStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ArchiveStore for MemoryArchiveStore {
    fn ingest(&self, batch: IngestBatch) -> IngestResult {
        let mut entries = self.entries.lock().expect("lock poisoned");
        let accepted = batch.entries.len();
        for entry in batch.entries {
            entries.push(StoredEntry {
                entry,
                source_vm: batch.source_vm.clone(),
            });
        }
        IngestResult { accepted, rejected: 0 }
    }

    fn query(&self, query: &ArchiveQuery) -> Vec<LogEntry> {
        let entries = self.entries.lock().expect("lock poisoned");
        entries
            .iter()
            .filter(|se| {
                if let Some(ref vm) = query.vm {
                    if &se.source_vm != vm {
                        return false;
                    }
                }
                if let Some(ref op) = query.operator_id {
                    if se.entry.operator_id.as_ref() != Some(op) {
                        return false;
                    }
                }
                if let Some(min) = query.min_level {
                    if se.entry.level < min {
                        return false;
                    }
                }
                true
            })
            .rev()
            .take(query.limit)
            .map(|se| se.entry.clone())
            .collect()
    }

    fn query_by_operator(&self, operator_id: &str) -> Vec<LogEntry> {
        let entries = self.entries.lock().expect("lock poisoned");
        entries
            .iter()
            .filter(|se| se.entry.operator_id.as_deref() == Some(operator_id))
            .map(|se| se.entry.clone())
            .collect()
    }

    fn stats(&self) -> ArchiveStats {
        let entries = self.entries.lock().expect("lock poisoned");
        let mut by_vm: HashMap<String, usize> = HashMap::new();
        let mut by_level: HashMap<String, usize> = HashMap::new();
        for se in entries.iter() {
            *by_vm.entry(se.source_vm.clone()).or_default() += 1;
            *by_level.entry(se.entry.level.to_string()).or_default() += 1;
        }
        ArchiveStats {
            total_entries: entries.len(),
            by_vm,
            by_level,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_batch(vm: &str, entries: Vec<LogEntry>) -> IngestBatch {
        IngestBatch {
            entries,
            source_vm: vm.to_string(),
        }
    }

    #[test]
    fn test_ingest_and_stats() {
        let store = MemoryArchiveStore::new();
        let batch = make_batch(
            "vm-1",
            vec![LogEntry::new("svc", LogLevel::Info, "a"), LogEntry::new("svc", LogLevel::Warn, "b")],
        );
        let result = store.ingest(batch);
        assert_eq!(result.accepted, 2);
        assert_eq!(result.rejected, 0);

        let stats = store.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.by_vm.get("vm-1"), Some(&2));
    }

    #[test]
    fn test_query_all() {
        let store = MemoryArchiveStore::new();
        store.ingest(make_batch("vm-1", vec![LogEntry::new("svc", LogLevel::Info, "a")]));
        store.ingest(make_batch("vm-2", vec![LogEntry::new("svc", LogLevel::Warn, "b")]));

        let results = store.query(&ArchiveQuery::default());
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_query_filter_by_vm() {
        let store = MemoryArchiveStore::new();
        store.ingest(make_batch("vm-1", vec![LogEntry::new("svc", LogLevel::Info, "a")]));
        store.ingest(make_batch("vm-2", vec![LogEntry::new("svc", LogLevel::Warn, "b")]));

        let results = store.query(&ArchiveQuery {
            vm: Some("vm-1".to_string()),
            ..Default::default()
        });
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_query_filter_by_operator() {
        let store = MemoryArchiveStore::new();
        store.ingest(make_batch(
            "vm-1",
            vec![
                LogEntry::new("svc", LogLevel::Info, "a").with_operator("op-1"),
                LogEntry::new("svc", LogLevel::Info, "b").with_operator("op-2"),
            ],
        ));

        let results = store.query(&ArchiveQuery {
            operator_id: Some("op-1".to_string()),
            ..Default::default()
        });
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_query_by_operator_method() {
        let store = MemoryArchiveStore::new();
        store.ingest(make_batch(
            "vm-1",
            vec![
                LogEntry::new("svc", LogLevel::Info, "a").with_operator("op-1"),
                LogEntry::new("svc", LogLevel::Warn, "b").with_operator("op-1"),
                LogEntry::new("svc", LogLevel::Error, "c").with_operator("op-2"),
            ],
        ));

        let results = store.query_by_operator("op-1");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_stats_by_level() {
        let store = MemoryArchiveStore::new();
        store.ingest(make_batch(
            "vm-1",
            vec![
                LogEntry::new("svc", LogLevel::Info, "a"),
                LogEntry::new("svc", LogLevel::Info, "b"),
                LogEntry::new("svc", LogLevel::Error, "c"),
            ],
        ));

        let stats = store.stats();
        assert_eq!(stats.by_level.get("info"), Some(&2));
        assert_eq!(stats.by_level.get("error"), Some(&1));
    }
}
