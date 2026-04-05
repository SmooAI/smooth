//! Core data types for the pearl tracker.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Pearl status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PearlStatus {
    Open,
    InProgress,
    Closed,
    Deferred,
}

impl fmt::Display for PearlStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let icon = match self {
            Self::Open => "\u{25CB}",       // ○
            Self::InProgress => "\u{25D0}", // ◐
            Self::Closed => "\u{2713}",     // ✓
            Self::Deferred => "\u{2744}",   // ❄
        };
        write!(f, "{icon}")
    }
}

impl PearlStatus {
    /// Parse from a string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "open" => Some(Self::Open),
            "in_progress" | "inprogress" | "in-progress" => Some(Self::InProgress),
            "closed" => Some(Self::Closed),
            "deferred" => Some(Self::Deferred),
            _ => None,
        }
    }

    /// Database string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Closed => "closed",
            Self::Deferred => "deferred",
        }
    }
}

/// Priority levels (lower = higher priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Priority {
    Critical = 0,
    High = 1,
    Medium = 2,
    Low = 3,
    Backlog = 4,
}

impl Priority {
    /// Parse from an integer.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Critical),
            1 => Some(Self::High),
            2 => Some(Self::Medium),
            3 => Some(Self::Low),
            4 => Some(Self::Backlog),
            _ => None,
        }
    }

    /// Numeric value.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Critical => "P0-critical",
            Self::High => "P1-high",
            Self::Medium => "P2-medium",
            Self::Low => "P3-low",
            Self::Backlog => "P4-backlog",
        };
        write!(f, "{label}")
    }
}

/// Pearl type / category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PearlType {
    Task,
    Bug,
    Feature,
    Epic,
}

impl PearlType {
    /// Database string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
            Self::Feature => "feature",
            Self::Epic => "epic",
        }
    }

    /// Parse from a string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "task" => Some(Self::Task),
            "bug" => Some(Self::Bug),
            "feature" => Some(Self::Feature),
            "epic" => Some(Self::Epic),
            _ => None,
        }
    }
}

impl fmt::Display for PearlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A full pearl record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pearl {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: PearlStatus,
    pub priority: Priority,
    pub pearl_type: PearlType,
    pub labels: Vec<String>,
    pub assigned_to: Option<String>,
    pub parent_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

/// Parameters for creating a new pearl.
#[derive(Debug, Clone)]
pub struct NewPearl {
    pub title: String,
    pub description: String,
    pub pearl_type: PearlType,
    pub priority: Priority,
    pub assigned_to: Option<String>,
    pub parent_id: Option<String>,
    pub labels: Vec<String>,
}

/// Partial update — `None` fields are left unchanged.
#[derive(Debug, Clone, Default)]
pub struct PearlUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<PearlStatus>,
    pub priority: Option<Priority>,
    pub pearl_type: Option<PearlType>,
    pub assigned_to: Option<Option<String>>,
    pub parent_id: Option<Option<String>>,
}

/// A comment on an pearl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PearlComment {
    pub id: String,
    pub pearl_id: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

/// PearlDependency relationship between issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PearlDependency {
    pub pearl_id: String,
    pub depends_on: String,
    pub dep_type: PearlDepType,
}

/// PearlDependency type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PearlDepType {
    Blocks,
    Related,
}

impl PearlDepType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::Related => "related",
        }
    }
}

/// History entry recording a change to an pearl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PearlHistoryEntry {
    pub id: String,
    pub pearl_id: String,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub changed_at: DateTime<Utc>,
}

/// Aggregate stats across all issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PearlStats {
    pub open: usize,
    pub in_progress: usize,
    pub closed: usize,
    pub deferred: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_status_display() {
        assert_eq!(format!("{}", PearlStatus::Open), "\u{25CB}");
        assert_eq!(format!("{}", PearlStatus::InProgress), "\u{25D0}");
        assert_eq!(format!("{}", PearlStatus::Closed), "\u{2713}");
        assert_eq!(format!("{}", PearlStatus::Deferred), "\u{2744}");
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Medium);
        assert!(Priority::Medium < Priority::Low);
        assert!(Priority::Low < Priority::Backlog);
    }

    #[test]
    fn test_pearl_type_serialization() {
        let json = serde_json::to_string(&PearlType::Feature).expect("serialize");
        assert_eq!(json, "\"feature\"");
        let parsed: PearlType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, PearlType::Feature);
    }

    #[test]
    fn test_issue_serialization_roundtrip() {
        let pearl = Pearl {
            id: "th-abc123".to_string(),
            title: "Test pearl".to_string(),
            description: "A description".to_string(),
            status: PearlStatus::Open,
            priority: Priority::Medium,
            pearl_type: PearlType::Task,
            labels: vec!["backend".to_string()],
            assigned_to: None,
            parent_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        };
        let json = serde_json::to_string(&pearl).expect("serialize");
        let parsed: Pearl = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, pearl.id);
        assert_eq!(parsed.title, pearl.title);
        assert_eq!(parsed.status, pearl.status);
        assert_eq!(parsed.priority, pearl.priority);
        assert_eq!(parsed.pearl_type, pearl.pearl_type);
        assert_eq!(parsed.labels, pearl.labels);
    }
}
