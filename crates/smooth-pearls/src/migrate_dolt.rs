//! One-shot import of a legacy `.smooth/dolt` pearl store into the SQLite
//! pearl database. Pearl th-d3e842.
//!
//! Reads every table through the `smooth-dolt` CLI (read-only, no server
//! attach) and inserts with `INSERT OR IGNORE`, so running it twice is a
//! no-op and the Dolt directory is never touched. Ids and timestamps are
//! preserved.
//!
//! ponytail: dolt shim — this module goes with `dolt.rs` in pearl th-c6ba83.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::dolt::SmoothDolt;
use crate::memory::Memory;
use crate::store::{parse_ts, PearlStore};
use crate::types::{Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStatus, PearlType, Priority};

/// Rows read from Dolt vs rows newly inserted, per table.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub pearls: (usize, usize),
    pub dependencies: (usize, usize),
    pub labels: (usize, usize),
    pub comments: (usize, usize),
    pub history: (usize, usize),
    pub memories: (usize, usize),
    pub config: (usize, usize),
}

impl MigrationReport {
    /// `(read, inserted)` per table, in display order.
    #[must_use]
    pub fn rows(&self) -> [(&'static str, (usize, usize)); 7] {
        [
            ("pearls", self.pearls),
            ("dependencies", self.dependencies),
            ("labels", self.labels),
            ("comments", self.comments),
            ("history", self.history),
            ("memories", self.memories),
            ("config", self.config),
        ]
    }
}

fn s(row: &Value, key: &str) -> String {
    row[key].as_str().unwrap_or_default().to_string()
}

fn opt_s(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(String::from)
}

fn ts(row: &Value, key: &str) -> DateTime<Utc> {
    row[key].as_str().and_then(parse_ts).unwrap_or_else(Utc::now)
}

fn opt_ts(row: &Value, key: &str) -> Option<DateTime<Utc>> {
    row[key].as_str().and_then(parse_ts)
}

fn pearl_from_row(row: &Value) -> Pearl {
    Pearl {
        id: s(row, "id"),
        title: s(row, "title"),
        description: s(row, "description"),
        status: PearlStatus::from_str_loose(&s(row, "status")).unwrap_or(PearlStatus::Open),
        priority: u8::try_from(row["priority"].as_u64().unwrap_or(2))
            .ok()
            .and_then(Priority::from_u8)
            .unwrap_or(Priority::Medium),
        pearl_type: PearlType::from_str_loose(&s(row, "pearl_type")).unwrap_or(PearlType::Task),
        labels: Vec::new(),
        assigned_to: opt_s(row, "assigned_to"),
        parent_id: opt_s(row, "parent_id"),
        created_at: ts(row, "created_at"),
        updated_at: ts(row, "updated_at"),
        closed_at: opt_ts(row, "closed_at"),
        scheduled_at: opt_ts(row, "scheduled_at"),
    }
}

/// Select every row of `table`, or an empty vec when the table is absent
/// (older stores predate `config`/`memories`).
fn rows(dolt: &SmoothDolt, table: &str) -> Result<Vec<Value>> {
    match dolt.sql(&format!("SELECT * FROM {table}")) {
        Ok(rows) => Ok(rows),
        Err(e) if format!("{e:#}").contains("table not found") => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read dolt table {table}")),
    }
}

/// Import the Dolt store at `dolt_dir` (a `.smooth/dolt` root) into `store`.
///
/// # Errors
/// Returns an error if the `smooth-dolt` binary is missing, the store
/// can't be read, or an insert fails.
pub fn migrate_from_dolt(dolt_dir: &Path, store: &PearlStore) -> Result<MigrationReport> {
    let dolt = SmoothDolt::new_cli_only(dolt_dir).with_context(|| format!("open dolt store {}", dolt_dir.display()))?;
    let mut report = MigrationReport::default();

    let pearls = rows(&dolt, "pearls")?;
    report.pearls.0 = pearls.len();
    for row in &pearls {
        if store.import_pearl(&pearl_from_row(row))? {
            report.pearls.1 += 1;
        }
    }

    let deps = rows(&dolt, "pearl_dependencies")?;
    report.dependencies.0 = deps.len();
    for row in &deps {
        let dep = PearlDependency {
            pearl_id: s(row, "pearl_id"),
            depends_on: s(row, "depends_on"),
            dep_type: if s(row, "dep_type") == "related" {
                PearlDepType::Related
            } else {
                PearlDepType::Blocks
            },
        };
        if store.import_dep(&dep)? {
            report.dependencies.1 += 1;
        }
    }

    let labels = rows(&dolt, "pearl_labels")?;
    report.labels.0 = labels.len();
    for row in &labels {
        if store.import_label(&s(row, "pearl_id"), &s(row, "label"))? {
            report.labels.1 += 1;
        }
    }

    let mut comments = rows(&dolt, "pearl_comments")?;
    // Keep Dolt's insertion order so the SQLite `seq` tiebreak matches.
    comments.sort_by_key(|r| r["seq"].as_i64().unwrap_or(0));
    report.comments.0 = comments.len();
    for row in &comments {
        let c = PearlComment {
            id: s(row, "id"),
            pearl_id: s(row, "pearl_id"),
            content: s(row, "content"),
            created_at: ts(row, "created_at"),
        };
        if store.import_comment(&c)? {
            report.comments.1 += 1;
        }
    }

    let mut history = rows(&dolt, "pearl_history")?;
    history.sort_by_key(|r| s(r, "changed_at"));
    report.history.0 = history.len();
    for row in &history {
        let h = PearlHistoryEntry {
            id: s(row, "id"),
            pearl_id: s(row, "pearl_id"),
            field: s(row, "field_name"),
            old_value: opt_s(row, "old_value"),
            new_value: opt_s(row, "new_value"),
            changed_at: ts(row, "changed_at"),
        };
        if store.import_history(&h)? {
            report.history.1 += 1;
        }
    }

    let memories = rows(&dolt, "memories")?;
    report.memories.0 = memories.len();
    let mem = store.memory();
    for row in &memories {
        let m = Memory {
            id: s(row, "id"),
            content: s(row, "content"),
            source: s(row, "source"),
            created_at: ts(row, "created_at"),
        };
        if mem.import(&m)? {
            report.memories.1 += 1;
        }
    }

    let config = rows(&dolt, "config")?;
    report.config.0 = config.len();
    for row in &config {
        if store.import_config(&s(row, "k"), &s(row, "v"), ts(row, "updated_at"))? {
            report.config.1 += 1;
        }
    }

    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::dolt::sql_escape;
    use crate::query::PearlQuery;

    /// A minimal legacy Dolt store, or `None` when `smooth-dolt` isn't on
    /// this machine (CI without the Go build) — the test then skips.
    fn legacy_store(dir: &Path) -> Option<SmoothDolt> {
        let cli = SmoothDolt::new_cli_only(dir).ok()?;
        cli.init().ok()?;
        for ddl in [
            "CREATE TABLE pearls (id VARCHAR(20) PRIMARY KEY, title TEXT NOT NULL, description TEXT DEFAULT '', status VARCHAR(20) NOT NULL DEFAULT 'open', priority INT NOT NULL DEFAULT 2, pearl_type VARCHAR(20) NOT NULL DEFAULT 'task', parent_id VARCHAR(20), assigned_to VARCHAR(100), created_at DATETIME DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME DEFAULT CURRENT_TIMESTAMP, closed_at DATETIME, scheduled_at DATETIME)",
            "CREATE TABLE pearl_dependencies (pearl_id VARCHAR(20) NOT NULL, depends_on VARCHAR(20) NOT NULL, dep_type VARCHAR(20) DEFAULT 'blocks', PRIMARY KEY (pearl_id, depends_on))",
            "CREATE TABLE pearl_labels (pearl_id VARCHAR(20) NOT NULL, label VARCHAR(100) NOT NULL, PRIMARY KEY (pearl_id, label))",
            "CREATE TABLE pearl_comments (id VARCHAR(20) PRIMARY KEY, pearl_id VARCHAR(20) NOT NULL, content TEXT NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP, seq BIGINT AUTO_INCREMENT UNIQUE)",
            "CREATE TABLE pearl_history (id VARCHAR(20) PRIMARY KEY, pearl_id VARCHAR(20) NOT NULL, field_name VARCHAR(50) NOT NULL, old_value TEXT, new_value TEXT, changed_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
            "CREATE TABLE memories (id VARCHAR(40) PRIMARY KEY, content TEXT NOT NULL, source VARCHAR(100), created_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
        ] {
            cli.exec(ddl).expect("legacy ddl");
        }
        let desc = sql_escape("it's got 'quotes' and 世界");
        for stmt in [
            format!("INSERT INTO pearls (id, title, description, status, priority, pearl_type, created_at, updated_at, closed_at) VALUES ('th-aaaaaa', 'A', '{desc}', 'closed', 1, 'bug', '2026-01-02 03:04:05', '2026-01-03 00:00:00', '2026-01-03 00:00:00')"),
            "INSERT INTO pearls (id, title, status, priority, pearl_type, created_at, updated_at) VALUES ('th-bbbbbb', 'B', 'open', 2, 'task', '2026-01-02 03:04:06', '2026-01-02 03:04:06')".to_string(),
            "INSERT INTO pearl_dependencies (pearl_id, depends_on, dep_type) VALUES ('th-bbbbbb', 'th-aaaaaa', 'blocks')".to_string(),
            "INSERT INTO pearl_labels (pearl_id, label) VALUES ('th-bbbbbb', 'backend')".to_string(),
            "INSERT INTO pearl_comments (id, pearl_id, content, created_at) VALUES ('th-cccccc', 'th-bbbbbb', 'first', '2026-01-02 03:05:00')".to_string(),
            "INSERT INTO pearl_history (id, pearl_id, field_name, old_value, new_value, changed_at) VALUES ('th-dddddd', 'th-aaaaaa', 'status', 'open', 'closed', '2026-01-03 00:00:00')".to_string(),
            "INSERT INTO memories (id, content, source, created_at) VALUES ('mem-eeeeee', 'remember me', 'manual', '2026-01-04 00:00:00')".to_string(),
        ] {
            cli.exec(&stmt).expect("legacy insert");
        }
        Some(cli)
    }

    #[test]
    fn imports_every_table_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dolt_dir = tmp.path().join(".smooth/dolt");
        if legacy_store(&dolt_dir).is_none() {
            eprintln!("skipping: smooth-dolt binary not available");
            return;
        }
        let store = PearlStore::open_with_db(&tmp.path().join("pearls.db"), tmp.path()).unwrap();

        let report = migrate_from_dolt(&dolt_dir, &store).expect("migrate");
        assert_eq!(report.pearls, (2, 2));
        assert_eq!(report.dependencies, (1, 1));
        assert_eq!(report.labels, (1, 1));
        assert_eq!(report.comments, (1, 1));
        assert_eq!(report.history, (1, 1));
        assert_eq!(report.memories, (1, 1));
        assert_eq!(report.config, (0, 0), "absent table reads as empty");

        let a = store.get("th-aaaaaa").unwrap().expect("imported");
        assert_eq!(a.status, PearlStatus::Closed);
        assert_eq!(a.priority, Priority::High);
        assert_eq!(a.pearl_type, PearlType::Bug);
        assert_eq!(a.description, "it's got 'quotes' and 世界");
        assert_eq!(a.created_at, parse_ts("2026-01-02 03:04:05").unwrap(), "timestamps preserved");
        assert!(a.closed_at.is_some());
        let b = store.get("th-bbbbbb").unwrap().unwrap();
        assert_eq!(b.labels, vec!["backend".to_string()]);
        assert_eq!(store.get_deps("th-bbbbbb").unwrap()[0].depends_on, "th-aaaaaa");
        assert_eq!(store.get_comments("th-bbbbbb").unwrap()[0].id, "th-cccccc");
        assert_eq!(store.get_history("th-aaaaaa").unwrap()[0].id, "th-dddddd");
        assert_eq!(store.memory().list_recent(5).unwrap()[0].id, "mem-eeeeee");
        // B depends on closed A → ready.
        assert_eq!(store.ready().unwrap().len(), 1);
        assert_eq!(store.list(&PearlQuery::new()).unwrap().len(), 2);

        // Second run inserts nothing, changes nothing.
        let again = migrate_from_dolt(&dolt_dir, &store).expect("re-migrate");
        assert_eq!(again.pearls, (2, 0));
        assert_eq!(again.comments, (1, 0));
        assert_eq!(store.stats().unwrap().total, 2);
    }

    #[test]
    fn pearl_from_row_tolerates_missing_and_odd_fields() {
        let p = pearl_from_row(&serde_json::json!({"id": "th-x", "title": "t", "priority": 99, "status": "bogus"}));
        assert_eq!(p.id, "th-x");
        assert_eq!(p.priority, Priority::Medium);
        assert_eq!(p.status, PearlStatus::Open);
        assert!(p.closed_at.is_none());
    }
}
