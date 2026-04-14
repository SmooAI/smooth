//! Query builder for filtering issues.

use crate::types::{PearlStatus, PearlType, Priority};

/// Query parameters for listing/filtering issues.
#[derive(Debug, Clone, Default)]
pub struct PearlQuery {
    pub status: Option<PearlStatus>,
    pub priority: Option<Priority>,
    pub pearl_type: Option<PearlType>,
    pub label: Option<String>,
    pub assigned_to: Option<String>,
    pub parent_id: Option<String>,
    pub limit: usize,
}

impl PearlQuery {
    /// Create a new query with default limit of 100.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limit: 100,
            ..Default::default()
        }
    }

    /// Filter by status.
    #[must_use]
    pub fn with_status(mut self, status: PearlStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Filter by priority.
    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Filter by pearl type.
    #[must_use]
    pub fn with_type(mut self, pearl_type: PearlType) -> Self {
        self.pearl_type = Some(pearl_type);
        self
    }

    /// Filter by label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Filter by assignee.
    #[must_use]
    pub fn with_assigned_to(mut self, assigned_to: impl Into<String>) -> Self {
        self.assigned_to = Some(assigned_to.into());
        self
    }

    /// Filter by parent pearl.
    #[must_use]
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// Set the result limit. Pass `0` for "no limit" (useful for the
    /// web UI where truncation is wrong but LLM tool calls need the cap).
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}
