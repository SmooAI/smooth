use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An archived event from the Smooth system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedEvent {
    pub id: String,
    pub event_type: String,
    pub task_id: Option<String>,
    pub operator_id: Option<String>,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

/// Filter criteria for querying archived events.
#[derive(Debug, Clone, Deserialize)]
pub struct EventFilter {
    pub task_id: Option<String>,
    pub operator_id: Option<String>,
    pub event_type: Option<String>,
    pub since: Option<DateTime<Utc>>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    100
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            task_id: None,
            operator_id: None,
            event_type: None,
            since: None,
            limit: 100,
        }
    }
}

/// Storage backend for archived events.
pub trait EventArchive: Send + Sync {
    /// Store a single event in the archive.
    ///
    /// # Errors
    ///
    /// Returns an error if the event could not be persisted.
    fn store(&self, event: &ArchivedEvent) -> anyhow::Result<()>;

    /// Query events matching the given filter.
    ///
    /// # Errors
    ///
    /// Returns an error if the query could not be executed.
    fn query(&self, filter: &EventFilter) -> anyhow::Result<Vec<ArchivedEvent>>;

    /// Count events matching the given filter.
    ///
    /// # Errors
    ///
    /// Returns an error if the count could not be computed.
    fn count(&self, filter: &EventFilter) -> anyhow::Result<usize>;
}

/// In-memory event archive (for testing and small deployments).
#[derive(Debug, Default)]
pub struct MemoryEventArchive {
    events: Mutex<Vec<ArchivedEvent>>,
}

impl MemoryEventArchive {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn matches(event: &ArchivedEvent, filter: &EventFilter) -> bool {
        if let Some(ref tid) = filter.task_id {
            if event.task_id.as_ref() != Some(tid) {
                return false;
            }
        }
        if let Some(ref oid) = filter.operator_id {
            if event.operator_id.as_ref() != Some(oid) {
                return false;
            }
        }
        if let Some(ref et) = filter.event_type {
            if &event.event_type != et {
                return false;
            }
        }
        if let Some(since) = filter.since {
            if event.timestamp < since {
                return false;
            }
        }
        true
    }
}

impl EventArchive for MemoryEventArchive {
    fn store(&self, event: &ArchivedEvent) -> anyhow::Result<()> {
        let mut events = self.events.lock().expect("lock poisoned");
        events.push(event.clone());
        Ok(())
    }

    fn query(&self, filter: &EventFilter) -> anyhow::Result<Vec<ArchivedEvent>> {
        let events = self.events.lock().expect("lock poisoned");
        let results: Vec<ArchivedEvent> = events.iter().filter(|e| Self::matches(e, filter)).rev().take(filter.limit).cloned().collect();
        Ok(results)
    }

    fn count(&self, filter: &EventFilter) -> anyhow::Result<usize> {
        let events = self.events.lock().expect("lock poisoned");
        let count = events.iter().filter(|e| Self::matches(e, filter)).count();
        Ok(count)
    }
}

/// Subscribe to a broadcast channel of JSON events and archive them.
/// Returns a join handle for the background archival task.
pub fn archive_from_broadcast(
    archive: std::sync::Arc<dyn EventArchive>,
    mut rx: tokio::sync::broadcast::Receiver<serde_json::Value>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event_json) = rx.recv().await {
            let event_type = event_json["type"].as_str().unwrap_or("unknown").to_string();
            let archived = ArchivedEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type,
                task_id: event_json["task_id"].as_str().map(String::from),
                operator_id: event_json["operator_id"].as_str().map(String::from),
                payload: event_json,
                timestamp: Utc::now(),
            };
            if let Err(e) = archive.store(&archived) {
                tracing::warn!(error = %e, "failed to archive event");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn make_event(event_type: &str, task_id: Option<&str>, operator_id: Option<&str>) -> ArchivedEvent {
        ArchivedEvent {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            task_id: task_id.map(String::from),
            operator_id: operator_id.map(String::from),
            payload: serde_json::json!({"type": event_type}),
            timestamp: Utc::now(),
        }
    }

    fn make_event_at(event_type: &str, timestamp: DateTime<Utc>) -> ArchivedEvent {
        ArchivedEvent {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            task_id: None,
            operator_id: None,
            payload: serde_json::json!({"type": event_type}),
            timestamp,
        }
    }

    #[test]
    fn archived_event_serialization_roundtrip() {
        let event = make_event("TokenDelta", Some("task-1"), Some("op-1"));
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: ArchivedEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.event_type, "TokenDelta");
        assert_eq!(parsed.task_id.as_deref(), Some("task-1"));
        assert_eq!(parsed.operator_id.as_deref(), Some("op-1"));
    }

    #[test]
    fn memory_archive_store_and_query_all() {
        let archive = MemoryEventArchive::new();
        archive.store(&make_event("TokenDelta", None, None)).expect("store");
        archive.store(&make_event("TaskComplete", None, None)).expect("store");

        let results = archive.query(&EventFilter::default()).expect("query");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn query_filter_by_task_id() {
        let archive = MemoryEventArchive::new();
        archive.store(&make_event("TokenDelta", Some("task-1"), None)).expect("store");
        archive.store(&make_event("TokenDelta", Some("task-2"), None)).expect("store");
        archive.store(&make_event("TaskComplete", None, None)).expect("store");

        let results = archive
            .query(&EventFilter {
                task_id: Some("task-1".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn query_filter_by_event_type() {
        let archive = MemoryEventArchive::new();
        archive.store(&make_event("NarcAlert", None, None)).expect("store");
        archive.store(&make_event("TokenDelta", None, None)).expect("store");
        archive.store(&make_event("NarcAlert", None, None)).expect("store");

        let results = archive
            .query(&EventFilter {
                event_type: Some("NarcAlert".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.event_type == "NarcAlert"));
    }

    #[test]
    fn query_filter_by_since() {
        let archive = MemoryEventArchive::new();
        let old = Utc::now() - Duration::hours(2);
        let recent = Utc::now() - Duration::minutes(5);

        archive.store(&make_event_at("Old", old)).expect("store");
        archive.store(&make_event_at("Recent", recent)).expect("store");
        archive.store(&make_event_at("Now", Utc::now())).expect("store");

        let cutoff = Utc::now() - Duration::hours(1);
        let results = archive
            .query(&EventFilter {
                since: Some(cutoff),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.event_type != "Old"));
    }

    #[test]
    fn query_with_limit() {
        let archive = MemoryEventArchive::new();
        for i in 0..10 {
            archive.store(&make_event(&format!("Event{i}"), None, None)).expect("store");
        }

        let results = archive
            .query(&EventFilter {
                limit: 3,
                ..Default::default()
            })
            .expect("query");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn count_returns_correct_number() {
        let archive = MemoryEventArchive::new();
        archive.store(&make_event("NarcAlert", Some("task-1"), None)).expect("store");
        archive.store(&make_event("TokenDelta", Some("task-1"), None)).expect("store");
        archive.store(&make_event("NarcAlert", Some("task-2"), None)).expect("store");

        let count = archive
            .count(&EventFilter {
                task_id: Some("task-1".to_string()),
                ..Default::default()
            })
            .expect("count");
        assert_eq!(count, 2);

        let total = archive.count(&EventFilter::default()).expect("count");
        assert_eq!(total, 3);
    }

    #[test]
    fn event_filter_default_has_limit_100() {
        let filter = EventFilter::default();
        assert_eq!(filter.limit, 100);
        assert!(filter.task_id.is_none());
        assert!(filter.operator_id.is_none());
        assert!(filter.event_type.is_none());
        assert!(filter.since.is_none());
    }
}
