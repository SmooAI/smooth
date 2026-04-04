//! Query builder for filtering issues.

use crate::types::{IssueStatus, IssueType, Priority};

/// Query parameters for listing/filtering issues.
#[derive(Debug, Clone, Default)]
pub struct IssueQuery {
    pub status: Option<IssueStatus>,
    pub priority: Option<Priority>,
    pub issue_type: Option<IssueType>,
    pub label: Option<String>,
    pub assigned_to: Option<String>,
    pub parent_id: Option<String>,
    pub limit: usize,
}

impl IssueQuery {
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
    pub fn with_status(mut self, status: IssueStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Filter by priority.
    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Filter by issue type.
    #[must_use]
    pub fn with_type(mut self, issue_type: IssueType) -> Self {
        self.issue_type = Some(issue_type);
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

    /// Filter by parent issue.
    #[must_use]
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// Set the result limit.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}
