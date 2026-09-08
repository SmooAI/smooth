//! Memory store — accumulating per-project notes the agent
//! writes during a task and reads back on subsequent dispatch.
//!
//! Pearl th-893801 Phase 3 iter-5a. Rows live in the `memories` table of
//! the pearl database, scoped by project like everything else:
//!
//! * `id` — short uuid (`mem-XXXXXX`).
//! * `content` — the note itself, free-form text.
//! * `source` — origin tag: a pearl id, an operator id,
//!   `"manual"`, etc. Used for filtering.
//! * `created_at` — insert time.
//!
//! The store is intentionally append-only. We don't edit individual
//! rows; the only ways to drop entries are `forget`, `clear_by_source`
//! and `clear_older_than`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{fmt_ts, now_ts, parse_ts, PearlStore};

/// A single learned-context note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    /// Origin tag — typically a pearl id (`"th-abc123"`),
    /// operator id, or a string like `"manual"` / `"system"`.
    /// Empty when not set.
    pub source: String,
    pub created_at: DateTime<Utc>,
}

/// Build a fresh memory id: `mem-` + 6 hex chars.
fn generate_id() -> String {
    let uuid = Uuid::new_v4();
    let hex = uuid.simple().to_string();
    format!("mem-{}", &hex[..6])
}

const COLS: &str = "id, content, source, created_at";

fn row_to_memory(r: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: r.get(0)?,
        content: r.get(1)?,
        source: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        created_at: parse_ts(&r.get::<_, String>(3)?).unwrap_or_else(Utc::now),
    })
}

/// API over the `memories` table. Cheap to clone (it shares the
/// underlying [`PearlStore`] connection).
#[derive(Clone)]
pub struct MemoryStore {
    store: PearlStore,
}

impl MemoryStore {
    /// Build a memory store over an existing pearl store (same project,
    /// same connection).
    #[must_use]
    pub fn new(store: PearlStore) -> Self {
        Self { store }
    }

    /// Append a memory. Returns the freshly-generated id.
    ///
    /// # Errors
    ///
    /// Returns an error if the content is blank or the insert fails.
    pub fn append(&self, content: impl Into<String>, source: impl Into<String>) -> Result<String> {
        let content = content.into();
        let source = source.into();
        if content.trim().is_empty() {
            anyhow::bail!("memory content must not be empty");
        }
        let id = generate_id();
        self.store
            .conn()
            .execute(
                "INSERT INTO memories (project, id, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![self.store.project(), id, content, source, now_ts()],
            )
            .context("insert memory row")?;
        Ok(id)
    }

    /// Import a memory verbatim, keeping its id + timestamp (idempotent).
    /// Used by `migrate-from-dolt`.
    ///
    /// # Errors
    ///
    /// Returns an error if the insert fails.
    pub fn import(&self, m: &Memory) -> Result<bool> {
        let n = self
            .store
            .conn()
            .execute(
                "INSERT OR IGNORE INTO memories (project, id, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![self.store.project(), m.id, m.content, m.source, fmt_ts(m.created_at)],
            )
            .context("import memory row")?;
        Ok(n > 0)
    }

    /// List the `limit` most-recent memories, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<Memory>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM memories WHERE project = ?1 ORDER BY created_at DESC, seq DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![self.store.project(), i64::try_from(limit).unwrap_or(i64::MAX)], row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("list_recent memories")
    }

    /// List memories filtered to a specific source, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn list_by_source(&self, source: &str, limit: usize) -> Result<Vec<Memory>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM memories WHERE project = ?1 AND source = ?2 ORDER BY created_at DESC, seq DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![self.store.project(), source, i64::try_from(limit).unwrap_or(i64::MAX)], row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("list_by_source memories")
    }

    /// Total row count for this project.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .store
            .conn()
            .query_row("SELECT COUNT(*) FROM memories WHERE project = ?1", params![self.store.project()], |r| r.get(0))
            .context("count memories")?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Drop every memory tagged with the given source.
    /// Returns how many rows were deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn clear_by_source(&self, source: &str) -> Result<usize> {
        let n = self
            .store
            .conn()
            .execute("DELETE FROM memories WHERE project = ?1 AND source = ?2", params![self.store.project(), source])
            .context("clear_by_source")?;
        Ok(n)
    }

    /// Drop a single memory by id. Returns `true` if a row matched.
    /// Backs `th pearls forget <id>`. Pearl th-202885.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let n = self
            .store
            .conn()
            .execute("DELETE FROM memories WHERE project = ?1 AND id = ?2", params![self.store.project(), id])
            .context("forget memory")?;
        Ok(n > 0)
    }

    /// Drop every memory older than `cutoff`. Returns how many
    /// rows were deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn clear_older_than(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        let n = self
            .store
            .conn()
            .execute(
                "DELETE FROM memories WHERE project = ?1 AND created_at < ?2",
                params![self.store.project(), fmt_ts(cutoff)],
            )
            .context("clear_older_than")?;
        Ok(n)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::store::tests::test_store;

    fn fresh_store() -> MemoryStore {
        test_store().memory()
    }

    #[test]
    fn append_and_list_recent_round_trips() {
        let store = fresh_store();
        assert_eq!(store.count().unwrap(), 0);
        let id1 = store.append("first note", "manual").unwrap();
        let id2 = store.append("second note", "th-abc").unwrap();
        let id3 = store.append("third note", "th-abc").unwrap();
        assert!(id1.starts_with("mem-"));
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);

        assert_eq!(store.count().unwrap(), 3);
        let recent = store.list_recent(10).unwrap();
        assert_eq!(recent.len(), 3);
        // Microsecond timestamps + seq tiebreak make same-instant inserts ordered.
        assert_eq!(recent[0].content, "third note");
        assert_eq!(recent[2].content, "first note");
    }

    #[test]
    fn list_by_source_filters_correctly() {
        let store = fresh_store();
        store.append("a", "pearl-x").unwrap();
        store.append("b", "pearl-y").unwrap();
        store.append("c", "pearl-x").unwrap();

        let xs = store.list_by_source("pearl-x", 10).unwrap();
        assert_eq!(xs.len(), 2);
        assert!(xs.iter().all(|m| m.source == "pearl-x"));
        let ys = store.list_by_source("pearl-y", 10).unwrap();
        assert_eq!(ys.len(), 1);
        assert_eq!(ys[0].content, "b");
        assert!(store.list_by_source("pearl-z", 10).unwrap().is_empty());
    }

    #[test]
    fn list_recent_honors_limit() {
        let store = fresh_store();
        for i in 0..7 {
            store.append(format!("note {i}"), "manual").unwrap();
        }
        assert_eq!(store.list_recent(3).unwrap().len(), 3);
    }

    #[test]
    fn clear_by_source_drops_matching_rows() {
        let store = fresh_store();
        store.append("keep", "system").unwrap();
        store.append("drop1", "pearl-x").unwrap();
        store.append("drop2", "pearl-x").unwrap();
        assert_eq!(store.count().unwrap(), 3);

        assert_eq!(store.clear_by_source("pearl-x").unwrap(), 2);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.list_recent(10).unwrap()[0].content, "keep");
    }

    #[test]
    fn forget_drops_one_row_and_reports_misses() {
        let store = fresh_store();
        let id = store.append("x", "manual").unwrap();
        assert!(store.forget(&id).unwrap());
        assert!(!store.forget(&id).unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn clear_older_than_drops_old_rows() {
        let store = fresh_store();
        store.append("ancient", "manual").unwrap();
        let future = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(store.clear_older_than(future).unwrap(), 1);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn empty_content_is_rejected() {
        let store = fresh_store();
        let err = store.append("   ", "manual").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn quotes_in_content_dont_break_insert() {
        let store = fresh_store();
        let id = store.append("it's a \"thing\"", "manual").unwrap();
        let row = store.list_recent(1).unwrap();
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].id, id);
        assert_eq!(row[0].content, "it's a \"thing\"");
    }

    #[test]
    fn memories_are_project_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("pearls.db");
        let a = PearlStore::open_with_db(&db, std::path::Path::new("/p/a")).unwrap().memory();
        let b = PearlStore::open_with_db(&db, std::path::Path::new("/p/b")).unwrap().memory();
        a.append("only a", "manual").unwrap();
        assert_eq!(a.count().unwrap(), 1);
        assert_eq!(b.count().unwrap(), 0);
        let m = &a.list_recent(1).unwrap()[0];
        assert!(b.import(m).unwrap());
        assert!(!b.import(m).unwrap());
        assert_eq!(b.count().unwrap(), 1);
    }
}
