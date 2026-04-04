//! Issue tracking — thin wrappers around `smooth_issues::IssueStore`.
//!
//! Replaces the old `beads` module that shelled out to the `bd` CLI.

use anyhow::Result;
use smooth_issues::{Comment, Issue, IssueQuery, IssueStats, IssueStatus, IssueStore, IssueType, IssueUpdate, NewIssue, Priority};

/// List issues with optional status filter.
pub fn list_issues(store: &IssueStore, status: Option<&str>) -> Result<Vec<Issue>> {
    let query = match status {
        Some(s) => IssueQuery::new().with_status(IssueStatus::from_str_loose(s).unwrap_or(IssueStatus::Open)),
        None => IssueQuery::new(),
    };
    store.list(&query)
}

/// Get ready issues (open, no unresolved blockers).
pub fn get_ready(store: &IssueStore) -> Result<Vec<Issue>> {
    store.ready()
}

/// Get a specific issue by ID.
pub fn get_issue(store: &IssueStore, id: &str) -> Result<Option<Issue>> {
    store.get(id)
}

/// Create a new issue.
pub fn create_issue(store: &IssueStore, title: &str, description: &str, issue_type: &str, priority: u8) -> Result<Issue> {
    let new = NewIssue {
        title: title.to_string(),
        description: description.to_string(),
        issue_type: IssueType::from_str_loose(issue_type).unwrap_or(IssueType::Task),
        priority: Priority::from_u8(priority).unwrap_or(Priority::Medium),
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new)
}

/// Update an issue's status.
pub fn update_issue_status(store: &IssueStore, id: &str, status: &str) -> Result<Issue> {
    let update = IssueUpdate {
        status: IssueStatus::from_str_loose(status),
        ..Default::default()
    };
    store.update(id, &update)
}

/// Close one or more issues.
pub fn close_issues(store: &IssueStore, ids: &[&str]) -> Result<usize> {
    store.close(ids)
}

/// Add a comment to an issue.
pub fn add_comment(store: &IssueStore, issue_id: &str, content: &str) -> Result<Comment> {
    store.add_comment(issue_id, content)
}

/// Get comments for an issue.
pub fn get_comments(store: &IssueStore, issue_id: &str) -> Result<Vec<Comment>> {
    store.get_comments(issue_id)
}

/// Get aggregate stats.
pub fn stats(store: &IssueStore) -> Result<IssueStats> {
    store.stats()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> IssueStore {
        IssueStore::open_in_memory().unwrap()
    }

    #[test]
    fn test_list_issues_empty() {
        let store = test_store();
        let issues = list_issues(&store, None).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn test_create_and_list() {
        let store = test_store();
        let issue = create_issue(&store, "Test issue", "desc", "task", 2).unwrap();
        assert_eq!(issue.title, "Test issue");

        let all = list_issues(&store, None).unwrap();
        assert_eq!(all.len(), 1);

        let open = list_issues(&store, Some("open")).unwrap();
        assert_eq!(open.len(), 1);

        let closed = list_issues(&store, Some("closed")).unwrap();
        assert!(closed.is_empty());
    }

    #[test]
    fn test_get_ready() {
        let store = test_store();
        create_issue(&store, "Ready issue", "", "task", 2).unwrap();
        let ready = get_ready(&store).unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn test_get_issue() {
        let store = test_store();
        let created = create_issue(&store, "Find me", "", "task", 2).unwrap();
        let found = get_issue(&store, &created.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Find me");

        let missing = get_issue(&store, "th-000000").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_update_status() {
        let store = test_store();
        let issue = create_issue(&store, "Update me", "", "task", 2).unwrap();
        let updated = update_issue_status(&store, &issue.id, "in_progress").unwrap();
        assert_eq!(updated.status, IssueStatus::InProgress);
    }

    #[test]
    fn test_close_issues() {
        let store = test_store();
        let issue = create_issue(&store, "Close me", "", "task", 2).unwrap();
        let count = close_issues(&store, &[&issue.id]).unwrap();
        assert_eq!(count, 1);

        let closed = get_issue(&store, &issue.id).unwrap().unwrap();
        assert_eq!(closed.status, IssueStatus::Closed);
    }

    #[test]
    fn test_add_and_get_comments() {
        let store = test_store();
        let issue = create_issue(&store, "Commented", "", "task", 2).unwrap();
        add_comment(&store, &issue.id, "Hello").unwrap();
        add_comment(&store, &issue.id, "World").unwrap();

        let comments = get_comments(&store, &issue.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].content, "Hello");
        assert_eq!(comments[1].content, "World");
    }

    #[test]
    fn test_stats() {
        let store = test_store();
        create_issue(&store, "One", "", "task", 2).unwrap();
        let two = create_issue(&store, "Two", "", "task", 2).unwrap();
        close_issues(&store, &[&two.id]).unwrap();

        let s = stats(&store).unwrap();
        assert_eq!(s.open, 1);
        assert_eq!(s.closed, 1);
        assert_eq!(s.total, 2);
    }
}
