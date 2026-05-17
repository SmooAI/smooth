//! Memory store — accumulating per-project notes the agent
//! writes during a task and reads back on subsequent dispatch.
//!
//! Pearl th-893801 Phase 3 iter-5a. The `memories` table has
//! lived in the pearl Dolt DB schema since the start but had
//! no API; this module supplies CRUD on top of it. Each row:
//!
//! * `id` — short uuid (`mem-XXXXXX`).
//! * `content` — the note itself, free-form text.
//! * `source` — origin tag: a pearl id, an operator id,
//!   `"manual"`, etc. Used for filtering.
//! * `created_at` — insert time.
//!
//! The store is intentionally append-only. We don't delete or
//! edit individual rows; the only way to drop entries is
//! `clear_by_source` or `clear_older_than`. Long-term we'd
//! add a summarize-and-collapse pass.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::dolt::SmoothDolt;

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

/// Parse a Dolt JSON row into a `Memory`.
fn parse_memory(row: &Value) -> Memory {
    Memory {
        id: row["id"].as_str().unwrap_or_default().to_string(),
        content: row["content"].as_str().unwrap_or_default().to_string(),
        source: row["source"].as_str().unwrap_or_default().to_string(),
        created_at: parse_datetime(&row["created_at"]),
    }
}

fn parse_datetime(value: &Value) -> DateTime<Utc> {
    let s = value.as_str().unwrap_or_default();
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return naive.and_utc();
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return naive.and_utc();
    }
    Utc::now()
}

/// SQL-safe escape for string literals — Dolt's smooth-dolt
/// CLI doesn't expose prepared statements; doubling single
/// quotes is the standard SQL escape.
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// API over the `memories` table. Cheap to clone (the
/// underlying SmoothDolt is itself cheap to clone).
#[derive(Clone)]
pub struct MemoryStore {
    dolt: SmoothDolt,
}

impl MemoryStore {
    /// Build a store from an existing SmoothDolt handle. The
    /// `memories` table is created by `PearlStore::open`/`init`
    /// so the caller has already ensured it exists.
    #[must_use]
    pub fn new(dolt: SmoothDolt) -> Self {
        Self { dolt }
    }

    /// Append a memory. Returns the freshly-generated id.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt insert fails.
    pub fn append(&self, content: impl Into<String>, source: impl Into<String>) -> Result<String> {
        let content = content.into();
        let source = source.into();
        if content.trim().is_empty() {
            anyhow::bail!("memory content must not be empty");
        }
        let id = generate_id();
        let sql = format!(
            "INSERT INTO memories (id, content, source) VALUES ('{}', '{}', '{}')",
            sql_escape(&id),
            sql_escape(&content),
            sql_escape(&source),
        );
        self.dolt.exec(&sql).context("insert memory row")?;
        Ok(id)
    }

    /// List the `limit` most-recent memories, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt query fails.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<Memory>> {
        let sql = format!("SELECT id, content, source, created_at FROM memories ORDER BY created_at DESC, id DESC LIMIT {limit}");
        let rows = self.dolt.sql(&sql).context("list_recent memories")?;
        Ok(rows.iter().map(parse_memory).collect())
    }

    /// List memories filtered to a specific source, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt query fails.
    pub fn list_by_source(&self, source: &str, limit: usize) -> Result<Vec<Memory>> {
        let sql = format!(
            "SELECT id, content, source, created_at FROM memories WHERE source = '{}' ORDER BY created_at DESC, id DESC LIMIT {limit}",
            sql_escape(source),
        );
        let rows = self.dolt.sql(&sql).context("list_by_source memories")?;
        Ok(rows.iter().map(parse_memory).collect())
    }

    /// Total row count.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt query fails.
    pub fn count(&self) -> Result<usize> {
        let rows = self.dolt.sql("SELECT COUNT(*) AS n FROM memories").context("count memories")?;
        let n = rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0);
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Drop every memory tagged with the given source.
    /// Returns how many rows were deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt delete fails.
    pub fn clear_by_source(&self, source: &str) -> Result<usize> {
        let before = self.count_for_source(source)?;
        let sql = format!("DELETE FROM memories WHERE source = '{}'", sql_escape(source));
        self.dolt.exec(&sql).context("clear_by_source")?;
        Ok(before)
    }

    fn count_for_source(&self, source: &str) -> Result<usize> {
        let sql = format!("SELECT COUNT(*) AS n FROM memories WHERE source = '{}'", sql_escape(source));
        let rows = self.dolt.sql(&sql).context("count_for_source")?;
        let n = rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0);
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Drop every memory older than `cutoff`. Returns how many
    /// rows were deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the Dolt delete fails.
    pub fn clear_older_than(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        let cutoff_sql = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();
        let count_sql = format!("SELECT COUNT(*) AS n FROM memories WHERE created_at < '{cutoff_sql}'");
        let n = self
            .dolt
            .sql(&count_sql)
            .context("count_older")?
            .first()
            .and_then(|r| r["n"].as_u64())
            .unwrap_or(0);
        let delete_sql = format!("DELETE FROM memories WHERE created_at < '{cutoff_sql}'");
        self.dolt.exec(&delete_sql).context("clear_older_than")?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::store::PearlStore;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Spin up a fresh Dolt-backed PearlStore in a tempdir and
    /// return its MemoryStore. Lets us exercise the real Dolt
    /// path without a long-lived dev DB.
    fn fresh_store() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = PearlStore::init(&tmp.path().join(".smooth/dolt")).expect("init pearl store");
        let memory = MemoryStore::new(store.dolt().clone());
        (tmp, memory)
    }

    #[test]
    fn append_and_list_recent_round_trips() {
        let (_tmp, store) = fresh_store();
        assert_eq!(store.count().unwrap(), 0);
        // Sleep ≥1s between inserts so the DATETIME column
        // (1-second resolution in Dolt) gives a deterministic
        // ordering. Production writes are normally seconds
        // apart so this isn't a real constraint.
        let id1 = store.append("first note", "manual").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let id2 = store.append("second note", "th-abc").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let id3 = store.append("third note", "th-abc").unwrap();
        assert!(id1.starts_with("mem-"));
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);

        assert_eq!(store.count().unwrap(), 3);
        let recent = store.list_recent(10).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "third note");
        assert_eq!(recent[2].content, "first note");
    }

    #[test]
    fn appends_within_same_second_are_all_retrievable() {
        // When two writes land in the same second the order
        // between them is unspecified, but `list_recent` still
        // returns every row. Most production callers care
        // about the recent-set, not the exact internal
        // ordering.
        let (_tmp, store) = fresh_store();
        store.append("a", "manual").unwrap();
        store.append("b", "manual").unwrap();
        store.append("c", "manual").unwrap();
        let recent = store.list_recent(10).unwrap();
        assert_eq!(recent.len(), 3);
        let contents: std::collections::HashSet<&str> = recent.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains("a"));
        assert!(contents.contains("b"));
        assert!(contents.contains("c"));
    }

    #[test]
    fn list_by_source_filters_correctly() {
        let (_tmp, store) = fresh_store();
        store.append("a", "pearl-x").unwrap();
        store.append("b", "pearl-y").unwrap();
        store.append("c", "pearl-x").unwrap();

        let xs = store.list_by_source("pearl-x", 10).unwrap();
        assert_eq!(xs.len(), 2);
        assert!(xs.iter().all(|m| m.source == "pearl-x"));
        let ys = store.list_by_source("pearl-y", 10).unwrap();
        assert_eq!(ys.len(), 1);
        assert_eq!(ys[0].content, "b");
        let none = store.list_by_source("pearl-z", 10).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn list_recent_honors_limit() {
        let (_tmp, store) = fresh_store();
        for i in 0..7 {
            store.append(format!("note {i}"), "manual").unwrap();
        }
        let three = store.list_recent(3).unwrap();
        assert_eq!(three.len(), 3);
    }

    #[test]
    fn clear_by_source_drops_matching_rows() {
        let (_tmp, store) = fresh_store();
        store.append("keep", "system").unwrap();
        store.append("drop1", "pearl-x").unwrap();
        store.append("drop2", "pearl-x").unwrap();
        assert_eq!(store.count().unwrap(), 3);

        let dropped = store.clear_by_source("pearl-x").unwrap();
        assert_eq!(dropped, 2);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.list_recent(10).unwrap()[0].content, "keep");
    }

    #[test]
    fn clear_older_than_drops_old_rows() {
        let (_tmp, store) = fresh_store();
        store.append("ancient", "manual").unwrap();
        // The cutoff is well in the future, so this drops
        // everything.
        let future = Utc::now() + chrono::Duration::hours(1);
        let dropped = store.clear_older_than(future).unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn empty_content_is_rejected() {
        let (_tmp, store) = fresh_store();
        let err = store.append("   ", "manual").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn sql_quotes_in_content_dont_break_insert() {
        let (_tmp, store) = fresh_store();
        let id = store.append("it's a \"thing\"", "manual").unwrap();
        let row = store.list_recent(1).unwrap();
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].id, id);
        assert_eq!(row[0].content, "it's a \"thing\"");
    }
}
