use serde::{Deserialize, Serialize};
use smooth_scribe::LogEntry;

/// A batch of log entries from a single Scribe instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestBatch {
    pub entries: Vec<LogEntry>,
    pub source_vm: String,
}

/// Result of ingesting a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub accepted: usize,
    pub rejected: usize,
}

#[cfg(test)]
mod tests {
    use smooth_scribe::LogLevel;

    use super::*;

    #[test]
    fn test_ingest_batch_serialization() {
        let batch = IngestBatch {
            entries: vec![LogEntry::new("svc", LogLevel::Info, "hello")],
            source_vm: "vm-1".to_string(),
        };
        let json = serde_json::to_string(&batch).expect("serialize");
        let deserialized: IngestBatch = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.source_vm, "vm-1");
        assert_eq!(deserialized.entries.len(), 1);
    }

    #[test]
    fn test_ingest_result_serialization() {
        let result = IngestResult { accepted: 5, rejected: 1 };
        let json = serde_json::to_string(&result).expect("serialize");
        let deserialized: IngestResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.accepted, 5);
        assert_eq!(deserialized.rejected, 1);
    }
}
