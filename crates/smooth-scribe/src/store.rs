use std::sync::Mutex;

use serde::Deserialize;

use crate::log_entry::{LogEntry, LogLevel};

/// Query parameters for filtering log entries.
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    pub service: Option<String>,
    pub min_level: Option<LogLevel>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            service: None,
            min_level: None,
            limit: 100,
        }
    }
}

fn default_limit() -> usize {
    100
}

/// Trait for a log entry storage backend.
pub trait LogStore: Send + Sync {
    /// Append a log entry to the store.
    fn append(&self, entry: LogEntry);

    /// Query entries matching the given filter.
    fn query(&self, query: &Query) -> Vec<LogEntry>;

    /// Return the total number of stored entries.
    fn count(&self) -> usize;
}

/// In-memory implementation of `LogStore`.
#[derive(Debug, Default)]
pub struct MemoryLogStore {
    entries: Mutex<Vec<LogEntry>>,
}

impl MemoryLogStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LogStore for MemoryLogStore {
    fn append(&self, entry: LogEntry) {
        let mut entries = self.entries.lock().expect("lock poisoned");
        entries.push(entry);
    }

    fn query(&self, query: &Query) -> Vec<LogEntry> {
        let entries = self.entries.lock().expect("lock poisoned");
        entries
            .iter()
            .filter(|e| {
                if let Some(ref svc) = query.service {
                    if &e.service != svc {
                        return false;
                    }
                }
                if let Some(min) = query.min_level {
                    if e.level < min {
                        return false;
                    }
                }
                true
            })
            .rev()
            .take(query.limit)
            .cloned()
            .collect()
    }

    fn count(&self) -> usize {
        self.entries.lock().expect("lock poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(service: &str, level: LogLevel, msg: &str) -> LogEntry {
        LogEntry::new(service, level, msg)
    }

    #[test]
    fn test_append_and_count() {
        let store = MemoryLogStore::new();
        assert_eq!(store.count(), 0);
        store.append(make_entry("svc", LogLevel::Info, "a"));
        store.append(make_entry("svc", LogLevel::Debug, "b"));
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn test_query_all() {
        let store = MemoryLogStore::new();
        store.append(make_entry("svc", LogLevel::Info, "a"));
        store.append(make_entry("svc", LogLevel::Warn, "b"));
        let results = store.query(&Query::default());
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_query_filter_by_service() {
        let store = MemoryLogStore::new();
        store.append(make_entry("alpha", LogLevel::Info, "a"));
        store.append(make_entry("beta", LogLevel::Info, "b"));
        store.append(make_entry("alpha", LogLevel::Warn, "c"));
        let results = store.query(&Query {
            service: Some("alpha".to_string()),
            ..Default::default()
        });
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.service == "alpha"));
    }

    #[test]
    fn test_query_filter_by_level() {
        let store = MemoryLogStore::new();
        store.append(make_entry("svc", LogLevel::Debug, "a"));
        store.append(make_entry("svc", LogLevel::Info, "b"));
        store.append(make_entry("svc", LogLevel::Warn, "c"));
        store.append(make_entry("svc", LogLevel::Error, "d"));
        let results = store.query(&Query {
            min_level: Some(LogLevel::Warn),
            ..Default::default()
        });
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.level >= LogLevel::Warn));
    }

    #[test]
    fn test_query_limit() {
        let store = MemoryLogStore::new();
        for i in 0..10 {
            store.append(make_entry("svc", LogLevel::Info, &format!("msg-{i}")));
        }
        let results = store.query(&Query {
            limit: 3,
            ..Default::default()
        });
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_default_query_limit() {
        let q = Query::default();
        assert_eq!(q.limit, 100);
    }
}
