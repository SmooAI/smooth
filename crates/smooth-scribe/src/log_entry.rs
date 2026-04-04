use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Severity level for a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Debug => write!(f, "debug"),
            Self::Info => write!(f, "info"),
            Self::Warn => write!(f, "warn"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// A structured log entry produced by a Scribe instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub service: String,
    pub level: LogLevel,
    pub message: String,
    pub fields: HashMap<String, String>,
    pub operator_id: Option<String>,
    pub bead_id: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl LogEntry {
    /// Create a new log entry with the given service, level, and message.
    /// Generates a unique ID and sets the timestamp to now.
    pub fn new(service: impl Into<String>, level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            service: service.into(),
            level,
            message: message.into(),
            fields: HashMap::new(),
            operator_id: None,
            bead_id: None,
            trace_id: None,
            span_id: None,
        }
    }

    /// Attach an operator ID to this entry.
    pub fn with_operator(mut self, operator_id: impl Into<String>) -> Self {
        self.operator_id = Some(operator_id.into());
        self
    }

    /// Attach a bead ID to this entry.
    pub fn with_bead(mut self, bead_id: impl Into<String>) -> Self {
        self.bead_id = Some(bead_id.into());
        self
    }

    /// Attach trace and span IDs to this entry.
    pub fn with_trace(mut self, trace_id: impl Into<String>, span_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self.span_id = Some(span_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_entry_has_id_and_timestamp() {
        let entry = LogEntry::new("test-service", LogLevel::Info, "hello");
        assert!(!entry.id.is_empty());
        assert_eq!(entry.service, "test-service");
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.message, "hello");
        assert!(entry.operator_id.is_none());
        assert!(entry.bead_id.is_none());
        assert!(entry.trace_id.is_none());
        assert!(entry.span_id.is_none());
    }

    #[test]
    fn test_with_operator() {
        let entry = LogEntry::new("svc", LogLevel::Debug, "msg").with_operator("op-1");
        assert_eq!(entry.operator_id.as_deref(), Some("op-1"));
    }

    #[test]
    fn test_with_bead() {
        let entry = LogEntry::new("svc", LogLevel::Warn, "msg").with_bead("bead-42");
        assert_eq!(entry.bead_id.as_deref(), Some("bead-42"));
    }

    #[test]
    fn test_with_trace() {
        let entry = LogEntry::new("svc", LogLevel::Error, "msg").with_trace("trace-1", "span-1");
        assert_eq!(entry.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(entry.span_id.as_deref(), Some("span-1"));
    }

    #[test]
    fn test_builder_chain() {
        let entry = LogEntry::new("svc", LogLevel::Info, "chained")
            .with_operator("op")
            .with_bead("bd")
            .with_trace("t", "s");
        assert_eq!(entry.operator_id.as_deref(), Some("op"));
        assert_eq!(entry.bead_id.as_deref(), Some("bd"));
        assert_eq!(entry.trace_id.as_deref(), Some("t"));
        assert_eq!(entry.span_id.as_deref(), Some("s"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let entry = LogEntry::new("svc", LogLevel::Info, "hello").with_operator("op-1");
        let json = serde_json::to_string(&entry).expect("serialize");
        let deserialized: LogEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, entry.id);
        assert_eq!(deserialized.service, entry.service);
        assert_eq!(deserialized.level, entry.level);
        assert_eq!(deserialized.message, entry.message);
        assert_eq!(deserialized.operator_id, entry.operator_id);
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(LogLevel::Debug.to_string(), "debug");
        assert_eq!(LogLevel::Info.to_string(), "info");
        assert_eq!(LogLevel::Warn.to_string(), "warn");
        assert_eq!(LogLevel::Error.to_string(), "error");
    }

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }
}
