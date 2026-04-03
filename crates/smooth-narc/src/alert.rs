use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Severity level for alerts, ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warn,
    Alert,
    Block,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warn => write!(f, "WARN"),
            Self::Alert => write!(f, "ALERT"),
            Self::Block => write!(f, "BLOCK"),
        }
    }
}

/// An alert raised by the Narc surveillance system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub message: String,
    pub tool_name: Option<String>,
    pub pattern_name: Option<String>,
    pub matched_text: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl Alert {
    /// Create a new alert with the given severity, category, and message.
    #[must_use]
    pub fn new(severity: Severity, category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            severity,
            category: category.into(),
            message: message.into(),
            tool_name: None,
            pattern_name: None,
            matched_text: None,
            timestamp: Utc::now(),
        }
    }

    /// Add tool context to the alert.
    #[must_use]
    pub fn with_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Add pattern match context to the alert.
    #[must_use]
    pub fn with_pattern(mut self, pattern_name: impl Into<String>, matched_text: impl Into<String>) -> Self {
        self.pattern_name = Some(pattern_name.into());
        self.matched_text = Some(matched_text.into());
        self
    }

    /// Returns true if this alert should block the operation.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.severity == Severity::Block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_creation() {
        let alert = Alert::new(Severity::Warn, "secrets", "Found potential secret");
        assert_eq!(alert.severity, Severity::Warn);
        assert_eq!(alert.category, "secrets");
        assert_eq!(alert.message, "Found potential secret");
        assert!(alert.tool_name.is_none());
        assert!(alert.pattern_name.is_none());
        assert!(alert.matched_text.is_none());
        assert!(!alert.id.is_empty());
    }

    #[test]
    fn alert_with_context() {
        let alert = Alert::new(Severity::Alert, "injection", "Prompt injection detected")
            .with_tool("shell_exec")
            .with_pattern("role_hijack", "ignore previous instructions");
        assert_eq!(alert.tool_name.as_deref(), Some("shell_exec"));
        assert_eq!(alert.pattern_name.as_deref(), Some("role_hijack"));
        assert_eq!(alert.matched_text.as_deref(), Some("ignore previous instructions"));
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Alert);
        assert!(Severity::Alert < Severity::Block);
    }

    #[test]
    fn alert_serialization() {
        let alert = Alert::new(Severity::Block, "write_guard", "Write blocked").with_tool("file_write");
        let json = serde_json::to_string(&alert).expect("serialize");
        let parsed: Alert = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.severity, Severity::Block);
        assert_eq!(parsed.category, "write_guard");
        assert_eq!(parsed.tool_name.as_deref(), Some("file_write"));
    }

    #[test]
    fn non_blocking_severity() {
        assert!(!Alert::new(Severity::Info, "test", "info").is_blocking());
        assert!(!Alert::new(Severity::Warn, "test", "warn").is_blocking());
        assert!(!Alert::new(Severity::Alert, "test", "alert").is_blocking());
        assert!(Alert::new(Severity::Block, "test", "block").is_blocking());
    }
}
