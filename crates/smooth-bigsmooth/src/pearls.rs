//! Pearl tracking — thin wrappers around `smooth_pearls::PearlStore`.
//!
//! Replaces the old `beads` module that shelled out to the `bd` CLI.

use anyhow::Result;
use smooth_pearls::{NewPearl, Pearl, PearlComment, PearlQuery, PearlStats, PearlStatus, PearlStore, PearlType, PearlUpdate, Priority};

/// List issues with optional status filter. Uses the default limit
/// (100) — suitable for LLM tool calls.
pub fn list_pearls(store: &PearlStore, status: Option<&str>) -> Result<Vec<Pearl>> {
    list_pearls_with_limit(store, status, 100)
}

/// List issues with optional status filter + explicit limit. Pass `0`
/// for "no limit" — the web UI uses this so client-side counts and
/// kanban views aren't cut off at 100.
pub fn list_pearls_with_limit(store: &PearlStore, status: Option<&str>, limit: usize) -> Result<Vec<Pearl>> {
    let mut query = PearlQuery::new().with_limit(limit);
    if let Some(s) = status {
        query = query.with_status(PearlStatus::from_str_loose(s).unwrap_or(PearlStatus::Open));
    }
    store.list(&query)
}

/// Get ready issues (open, no unresolved blockers).
pub fn get_ready(store: &PearlStore) -> Result<Vec<Pearl>> {
    store.ready()
}

/// Get a specific pearl by ID.
pub fn get_pearl(store: &PearlStore, id: &str) -> Result<Option<Pearl>> {
    store.get(id)
}

/// Create a new pearl.
pub fn create_pearl(store: &PearlStore, title: &str, description: &str, pearl_type: &str, priority: u8) -> Result<Pearl> {
    let new = NewPearl {
        title: title.to_string(),
        description: description.to_string(),
        pearl_type: PearlType::from_str_loose(pearl_type).unwrap_or(PearlType::Task),
        priority: Priority::from_u8(priority).unwrap_or(Priority::Medium),
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    store.create(&new)
}

/// Update an pearl's status.
pub fn update_pearl_status(store: &PearlStore, id: &str, status: &str) -> Result<Pearl> {
    let update = PearlUpdate {
        status: PearlStatus::from_str_loose(status),
        ..Default::default()
    };
    store.update(id, &update)
}

/// Close one or more issues.
pub fn close_pearls(store: &PearlStore, ids: &[&str]) -> Result<usize> {
    store.close(ids)
}

/// Add a comment to a pearl.
pub fn add_comment(store: &PearlStore, pearl_id: &str, content: &str) -> Result<PearlComment> {
    store.add_comment(pearl_id, content)
}

/// Get comments for an pearl.
pub fn get_comments(store: &PearlStore, pearl_id: &str) -> Result<Vec<PearlComment>> {
    store.get_comments(pearl_id)
}

/// Get aggregate stats.
pub fn stats(store: &PearlStore) -> Result<PearlStats> {
    store.stats()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Option<PearlStore> {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp);
                Some(store)
            }
            Err(_) => None,
        }
    }

    #[test]
    fn test_list_pearls_empty() {
        let Some(store) = test_store() else { return };
        let issues = list_pearls(&store, None).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn test_create_and_list() {
        let Some(store) = test_store() else { return };
        let pearl = create_pearl(&store, "Test pearl", "desc", "task", 2).unwrap();
        assert_eq!(pearl.title, "Test pearl");

        let all = list_pearls(&store, None).unwrap();
        assert_eq!(all.len(), 1);

        let open = list_pearls(&store, Some("open")).unwrap();
        assert_eq!(open.len(), 1);

        let closed = list_pearls(&store, Some("closed")).unwrap();
        assert!(closed.is_empty());
    }

    #[test]
    fn test_get_ready() {
        let Some(store) = test_store() else { return };
        create_pearl(&store, "Ready pearl", "", "task", 2).unwrap();
        let ready = get_ready(&store).unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn test_get_pearl() {
        let Some(store) = test_store() else { return };
        let created = create_pearl(&store, "Find me", "", "task", 2).unwrap();
        let found = get_pearl(&store, &created.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Find me");

        let missing = get_pearl(&store, "th-000000").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_update_status() {
        let Some(store) = test_store() else { return };
        let pearl = create_pearl(&store, "Update me", "", "task", 2).unwrap();
        let updated = update_pearl_status(&store, &pearl.id, "in_progress").unwrap();
        assert_eq!(updated.status, PearlStatus::InProgress);
    }

    #[test]
    fn test_close_pearls() {
        let Some(store) = test_store() else { return };
        let pearl = create_pearl(&store, "Close me", "", "task", 2).unwrap();
        let count = close_pearls(&store, &[&pearl.id]).unwrap();
        assert_eq!(count, 1);

        let closed = get_pearl(&store, &pearl.id).unwrap().unwrap();
        assert_eq!(closed.status, PearlStatus::Closed);
    }

    #[test]
    fn test_add_and_get_comments() {
        let Some(store) = test_store() else { return };
        let pearl = create_pearl(&store, "Commented", "", "task", 2).unwrap();
        add_comment(&store, &pearl.id, "Hello").unwrap();
        add_comment(&store, &pearl.id, "World").unwrap();

        let comments = get_comments(&store, &pearl.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].content, "Hello");
        assert_eq!(comments[1].content, "World");
    }

    #[test]
    fn test_stats() {
        let Some(store) = test_store() else { return };
        create_pearl(&store, "One", "", "task", 2).unwrap();
        let two = create_pearl(&store, "Two", "", "task", 2).unwrap();
        close_pearls(&store, &[&two.id]).unwrap();

        let s = stats(&store).unwrap();
        assert_eq!(s.open, 1);
        assert_eq!(s.closed, 1);
        assert_eq!(s.total, 2);
    }
}
