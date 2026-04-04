//! SQLite-backed issue store.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::query::IssueQuery;
use crate::types::{Comment, DepType, Dependency, HistoryEntry, Issue, IssueStats, IssueStatus, IssueType, IssueUpdate, NewIssue, Priority};

/// Thread-safe SQLite issue store.
#[derive(Clone)]
pub struct IssueStore {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    path: PathBuf,
}

/// Generate a short ID: "th-" + first 6 hex chars of a UUID v4.
fn generate_id() -> String {
    let uuid = Uuid::new_v4();
    let hex = uuid.simple().to_string();
    format!("th-{}", &hex[..6])
}

impl IssueStore {
    /// Open or create the issue store at the given path.
    pub fn open(path: &PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path.clone(),
        };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Open an in-memory store (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
        };
        store.ensure_schema()?;
        Ok(store)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))
    }

    /// Create all tables if they don't exist.
    fn ensure_schema(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS issues (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'open',
                priority INTEGER NOT NULL DEFAULT 2,
                issue_type TEXT NOT NULL DEFAULT 'task',
                assigned_to TEXT,
                parent_id TEXT,
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                closed_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS labels (
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                label TEXT NOT NULL,
                PRIMARY KEY (issue_id, label)
            );

            CREATE TABLE IF NOT EXISTS comments (
                id TEXT PRIMARY KEY,
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );

            CREATE TABLE IF NOT EXISTS dependencies (
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                depends_on TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                dep_type TEXT NOT NULL DEFAULT 'blocks',
                PRIMARY KEY (issue_id, depends_on)
            );

            CREATE TABLE IF NOT EXISTS history (
                id TEXT PRIMARY KEY,
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                field TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT,
                changed_at INTEGER NOT NULL DEFAULT (unixepoch())
            );

            CREATE INDEX IF NOT EXISTS idx_issues_status ON issues(status);
            CREATE INDEX IF NOT EXISTS idx_issues_priority ON issues(priority);
            CREATE INDEX IF NOT EXISTS idx_issues_type ON issues(issue_type);
            CREATE INDEX IF NOT EXISTS idx_labels_label ON labels(label);
            CREATE INDEX IF NOT EXISTS idx_comments_issue ON comments(issue_id);
            CREATE INDEX IF NOT EXISTS idx_deps_issue ON dependencies(issue_id);
            CREATE INDEX IF NOT EXISTS idx_deps_depends ON dependencies(depends_on);
            CREATE INDEX IF NOT EXISTS idx_history_issue ON history(issue_id);
            ",
        )?;
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn ts_to_dt(ts: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
    }

    fn row_to_issue(row: &rusqlite::Row<'_>) -> rusqlite::Result<Issue> {
        let status_str: String = row.get("status")?;
        let priority_val: u8 = row.get("priority")?;
        let type_str: String = row.get("issue_type")?;
        let closed_at: Option<i64> = row.get("closed_at")?;

        Ok(Issue {
            id: row.get("id")?,
            title: row.get("title")?,
            description: row.get("description")?,
            status: IssueStatus::from_str_loose(&status_str).unwrap_or(IssueStatus::Open),
            priority: Priority::from_u8(priority_val).unwrap_or(Priority::Medium),
            issue_type: IssueType::from_str_loose(&type_str).unwrap_or(IssueType::Task),
            labels: Vec::new(), // filled after query
            assigned_to: row.get("assigned_to")?,
            parent_id: row.get("parent_id")?,
            created_at: Self::ts_to_dt(row.get("created_at")?),
            updated_at: Self::ts_to_dt(row.get("updated_at")?),
            closed_at: closed_at.map(Self::ts_to_dt),
        })
    }

    fn load_labels(conn: &Connection, issue_id: &str) -> Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT label FROM labels WHERE issue_id = ?1 ORDER BY label")?;
        let labels: Vec<String> = stmt
            .query_map(params![issue_id], |row| row.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(labels)
    }

    fn load_issue_with_labels(conn: &Connection, mut issue: Issue) -> Result<Issue> {
        issue.labels = Self::load_labels(conn, &issue.id)?;
        Ok(issue)
    }

    // ── CRUD ─────────────────────────────────────────────────────────────

    /// Create a new issue.
    pub fn create(&self, new: &NewIssue) -> Result<Issue> {
        let id = generate_id();
        let now = Utc::now().timestamp();
        let conn = self.lock()?;

        conn.execute(
            "INSERT INTO issues (id, title, description, status, priority, issue_type, assigned_to, parent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                id,
                new.title,
                new.description,
                IssueStatus::Open.as_str(),
                new.priority.as_u8(),
                new.issue_type.as_str(),
                new.assigned_to,
                new.parent_id,
                now
            ],
        )?;

        for label in &new.labels {
            conn.execute("INSERT INTO labels (issue_id, label) VALUES (?1, ?2)", params![id, label])?;
        }

        let mut stmt = conn.prepare("SELECT * FROM issues WHERE id = ?1")?;
        let issue = stmt.query_row(params![id], Self::row_to_issue)?;
        drop(stmt);
        Self::load_issue_with_labels(&conn, issue)
    }

    /// Get an issue by ID.
    pub fn get(&self, id: &str) -> Result<Option<Issue>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT * FROM issues WHERE id = ?1")?;
        let result = stmt.query_row(params![id], Self::row_to_issue).ok();
        drop(stmt);
        match result {
            Some(issue) => Ok(Some(Self::load_issue_with_labels(&conn, issue)?)),
            None => Ok(None),
        }
    }

    /// List issues matching the given query.
    pub fn list(&self, query: &IssueQuery) -> Result<Vec<Issue>> {
        let conn = self.lock()?;

        let mut sql = String::from("SELECT i.* FROM issues i");
        let mut conditions: Vec<String> = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if query.label.is_some() {
            sql.push_str(" JOIN labels l ON l.issue_id = i.id");
        }

        if let Some(ref status) = query.status {
            conditions.push(format!("i.status = ?{param_idx}"));
            params_vec.push(Box::new(status.as_str().to_string()));
            param_idx += 1;
        }

        if let Some(ref priority) = query.priority {
            conditions.push(format!("i.priority = ?{param_idx}"));
            params_vec.push(Box::new(priority.as_u8()));
            param_idx += 1;
        }

        if let Some(ref issue_type) = query.issue_type {
            conditions.push(format!("i.issue_type = ?{param_idx}"));
            params_vec.push(Box::new(issue_type.as_str().to_string()));
            param_idx += 1;
        }

        if let Some(ref label) = query.label {
            conditions.push(format!("l.label = ?{param_idx}"));
            params_vec.push(Box::new(label.clone()));
            param_idx += 1;
        }

        if let Some(ref assigned_to) = query.assigned_to {
            conditions.push(format!("i.assigned_to = ?{param_idx}"));
            params_vec.push(Box::new(assigned_to.clone()));
            param_idx += 1;
        }

        if let Some(ref parent_id) = query.parent_id {
            conditions.push(format!("i.parent_id = ?{param_idx}"));
            params_vec.push(Box::new(parent_id.clone()));
            param_idx += 1;
        }

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        let _ = write!(sql, " ORDER BY i.priority ASC, i.created_at DESC LIMIT ?{param_idx}");
        #[allow(clippy::cast_possible_wrap)]
        params_vec.push(Box::new(query.limit as i64));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(std::convert::AsRef::as_ref).collect();

        let mut stmt = conn.prepare(&sql)?;
        let issues: Vec<Issue> = stmt
            .query_map(params_refs.as_slice(), Self::row_to_issue)?
            .filter_map(std::result::Result::ok)
            .collect();
        drop(stmt);

        let mut result = Vec::with_capacity(issues.len());
        for issue in issues {
            result.push(Self::load_issue_with_labels(&conn, issue)?);
        }
        Ok(result)
    }

    /// Update an issue with partial changes. Records history for each changed field.
    pub fn update(&self, id: &str, updates: &IssueUpdate) -> Result<Issue> {
        let conn = self.lock()?;

        // Load current state
        let mut stmt = conn.prepare("SELECT * FROM issues WHERE id = ?1")?;
        let current = stmt
            .query_row(params![id], Self::row_to_issue)
            .map_err(|_| anyhow::anyhow!("issue not found: {id}"))?;
        drop(stmt);

        let now = Utc::now().timestamp();
        let history_id = || generate_id();

        if let Some(ref title) = updates.title {
            if *title != current.title {
                conn.execute("UPDATE issues SET title = ?1, updated_at = ?2 WHERE id = ?3", params![title, now, id])?;
                conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'title', ?3, ?4, ?5)",
                    params![history_id(), id, current.title, title, now],
                )?;
            }
        }

        if let Some(ref desc) = updates.description {
            if *desc != current.description {
                conn.execute("UPDATE issues SET description = ?1, updated_at = ?2 WHERE id = ?3", params![desc, now, id])?;
                conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'description', ?3, ?4, ?5)",
                    params![history_id(), id, current.description, desc, now],
                )?;
            }
        }

        if let Some(ref status) = updates.status {
            if *status != current.status {
                let closed_at: Option<i64> = if *status == IssueStatus::Closed { Some(now) } else { None };
                conn.execute(
                    "UPDATE issues SET status = ?1, updated_at = ?2, closed_at = ?3 WHERE id = ?4",
                    params![status.as_str(), now, closed_at, id],
                )?;
                conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'status', ?3, ?4, ?5)",
                    params![history_id(), id, current.status.as_str(), status.as_str(), now],
                )?;
            }
        }

        if let Some(ref priority) = updates.priority {
            if *priority != current.priority {
                conn.execute(
                    "UPDATE issues SET priority = ?1, updated_at = ?2 WHERE id = ?3",
                    params![priority.as_u8(), now, id],
                )?;
                conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'priority', ?3, ?4, ?5)",
                    params![history_id(), id, current.priority.as_u8().to_string(), priority.as_u8().to_string(), now],
                )?;
            }
        }

        if let Some(ref issue_type) = updates.issue_type {
            if *issue_type != current.issue_type {
                conn.execute(
                    "UPDATE issues SET issue_type = ?1, updated_at = ?2 WHERE id = ?3",
                    params![issue_type.as_str(), now, id],
                )?;
                conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'issue_type', ?3, ?4, ?5)",
                    params![history_id(), id, current.issue_type.as_str(), issue_type.as_str(), now],
                )?;
            }
        }

        if let Some(ref assigned) = updates.assigned_to {
            conn.execute(
                "UPDATE issues SET assigned_to = ?1, updated_at = ?2 WHERE id = ?3",
                params![assigned.as_deref(), now, id],
            )?;
        }

        if let Some(ref parent) = updates.parent_id {
            conn.execute(
                "UPDATE issues SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
                params![parent.as_deref(), now, id],
            )?;
        }

        drop(conn);
        self.get(id)?.ok_or_else(|| anyhow::anyhow!("issue disappeared after update"))
    }

    /// Close one or more issues. Returns the number of issues actually closed.
    pub fn close(&self, ids: &[&str]) -> Result<usize> {
        let conn = self.lock()?;
        let now = Utc::now().timestamp();
        let mut count = 0;
        for id in ids {
            let changed = conn.execute(
                "UPDATE issues SET status = 'closed', closed_at = ?1, updated_at = ?1 WHERE id = ?2 AND status != 'closed'",
                params![now, id],
            )?;
            if changed > 0 {
                count += 1;
                let _ = conn.execute(
                    "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'status', 'open', 'closed', ?3)",
                    params![generate_id(), id, now],
                );
            }
        }
        Ok(count)
    }

    /// Reopen a closed issue.
    pub fn reopen(&self, id: &str) -> Result<Issue> {
        let conn = self.lock()?;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE issues SET status = 'open', closed_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        let _ = conn.execute(
            "INSERT INTO history (id, issue_id, field, old_value, new_value, changed_at) VALUES (?1, ?2, 'status', 'closed', 'open', ?3)",
            params![generate_id(), id, now],
        );
        drop(conn);
        self.get(id)?.ok_or_else(|| anyhow::anyhow!("issue not found: {id}"))
    }

    /// Delete an issue entirely.
    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM issues WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Dependencies ─────────────────────────────────────────────────────

    /// Add a blocking dependency: `issue_id` depends on `depends_on`.
    pub fn add_dep(&self, issue_id: &str, depends_on: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO dependencies (issue_id, depends_on, dep_type) VALUES (?1, ?2, ?3)",
            params![issue_id, depends_on, DepType::Blocks.as_str()],
        )?;
        Ok(())
    }

    /// Remove a dependency.
    pub fn remove_dep(&self, issue_id: &str, depends_on: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM dependencies WHERE issue_id = ?1 AND depends_on = ?2",
            params![issue_id, depends_on],
        )?;
        Ok(())
    }

    /// Get all issues that block the given issue (i.e., its unresolved blockers).
    pub fn get_blockers(&self, id: &str) -> Result<Vec<Issue>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT i.* FROM issues i
             JOIN dependencies d ON d.depends_on = i.id
             WHERE d.issue_id = ?1 AND d.dep_type = 'blocks' AND i.status != 'closed'",
        )?;
        let issues: Vec<Issue> = stmt.query_map(params![id], Self::row_to_issue)?.filter_map(std::result::Result::ok).collect();
        drop(stmt);

        let mut result = Vec::with_capacity(issues.len());
        for issue in issues {
            result.push(Self::load_issue_with_labels(&conn, issue)?);
        }
        Ok(result)
    }

    /// Get all dependencies for an issue.
    pub fn get_deps(&self, id: &str) -> Result<Vec<Dependency>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT issue_id, depends_on, dep_type FROM dependencies WHERE issue_id = ?1")?;
        let deps: Vec<Dependency> = stmt
            .query_map(params![id], |row| {
                let dep_type_str: String = row.get(2)?;
                Ok(Dependency {
                    issue_id: row.get(0)?,
                    depends_on: row.get(1)?,
                    dep_type: if dep_type_str == "related" { DepType::Related } else { DepType::Blocks },
                })
            })?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(deps)
    }

    // ── Labels ───────────────────────────────────────────────────────────

    /// Add a label to an issue.
    pub fn add_label(&self, id: &str, label: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?1, ?2)", params![id, label])?;
        Ok(())
    }

    /// Remove a label from an issue.
    pub fn remove_label(&self, id: &str, label: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM labels WHERE issue_id = ?1 AND label = ?2", params![id, label])?;
        Ok(())
    }

    // ── Comments ─────────────────────────────────────────────────────────

    /// Add a comment to an issue.
    pub fn add_comment(&self, issue_id: &str, content: &str) -> Result<Comment> {
        let id = generate_id();
        let now = Utc::now().timestamp();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO comments (id, issue_id, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, issue_id, content, now],
        )?;
        Ok(Comment {
            id,
            issue_id: issue_id.to_string(),
            content: content.to_string(),
            created_at: Self::ts_to_dt(now),
        })
    }

    /// Get all comments for an issue, ordered by creation time.
    pub fn get_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id, issue_id, content, created_at FROM comments WHERE issue_id = ?1 ORDER BY created_at ASC")?;
        let comments: Vec<Comment> = stmt
            .query_map(params![issue_id], |row| {
                let ts: i64 = row.get(3)?;
                Ok(Comment {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    content: row.get(2)?,
                    created_at: Self::ts_to_dt(ts),
                })
            })?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(comments)
    }

    // ── Query helpers ────────────────────────────────────────────────────

    /// Issues that are open with no unresolved blocking dependencies.
    pub fn ready(&self) -> Result<Vec<Issue>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT i.* FROM issues i
             WHERE i.status = 'open'
             AND NOT EXISTS (
                 SELECT 1 FROM dependencies d
                 JOIN issues blocker ON blocker.id = d.depends_on
                 WHERE d.issue_id = i.id AND d.dep_type = 'blocks' AND blocker.status != 'closed'
             )
             ORDER BY i.priority ASC, i.created_at DESC",
        )?;
        let issues: Vec<Issue> = stmt.query_map([], Self::row_to_issue)?.filter_map(std::result::Result::ok).collect();
        drop(stmt);

        let mut result = Vec::with_capacity(issues.len());
        for issue in issues {
            result.push(Self::load_issue_with_labels(&conn, issue)?);
        }
        Ok(result)
    }

    /// Issues that have unresolved blocking dependencies.
    pub fn blocked(&self) -> Result<Vec<Issue>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT i.* FROM issues i
             JOIN dependencies d ON d.issue_id = i.id
             JOIN issues blocker ON blocker.id = d.depends_on
             WHERE d.dep_type = 'blocks' AND blocker.status != 'closed' AND i.status != 'closed'
             ORDER BY i.priority ASC",
        )?;
        let issues: Vec<Issue> = stmt.query_map([], Self::row_to_issue)?.filter_map(std::result::Result::ok).collect();
        drop(stmt);

        let mut result = Vec::with_capacity(issues.len());
        for issue in issues {
            result.push(Self::load_issue_with_labels(&conn, issue)?);
        }
        Ok(result)
    }

    /// Full-text search on title and description (LIKE-based).
    pub fn search(&self, text: &str) -> Result<Vec<Issue>> {
        let conn = self.lock()?;
        let pattern = format!("%{text}%");
        let mut stmt = conn.prepare("SELECT * FROM issues WHERE title LIKE ?1 OR description LIKE ?1 ORDER BY priority ASC, created_at DESC")?;
        let issues: Vec<Issue> = stmt
            .query_map(params![pattern], Self::row_to_issue)?
            .filter_map(std::result::Result::ok)
            .collect();
        drop(stmt);

        let mut result = Vec::with_capacity(issues.len());
        for issue in issues {
            result.push(Self::load_issue_with_labels(&conn, issue)?);
        }
        Ok(result)
    }

    /// Aggregate stats across all issues.
    pub fn stats(&self) -> Result<IssueStats> {
        let conn = self.lock()?;
        let mut stats = IssueStats::default();

        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM issues GROUP BY status")?;
        let rows = stmt.query_map([], |row| {
            let status_val: String = row.get(0)?;
            let count_val: i64 = row.get(1)?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok((status_val, count_val.unsigned_abs() as usize))
        })?;

        for row in rows {
            let (status_val, count) = row?;
            match status_val.as_str() {
                "open" => stats.open = count,
                "in_progress" => stats.in_progress = count,
                "closed" => stats.closed = count,
                "deferred" => stats.deferred = count,
                _ => {}
            }
        }
        stats.total = stats.open + stats.in_progress + stats.closed + stats.deferred;
        Ok(stats)
    }

    // ── History ──────────────────────────────────────────────────────────

    /// Get change history for an issue.
    pub fn get_history(&self, issue_id: &str) -> Result<Vec<HistoryEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id, issue_id, field, old_value, new_value, changed_at FROM history WHERE issue_id = ?1 ORDER BY changed_at ASC")?;
        let entries: Vec<HistoryEntry> = stmt
            .query_map(params![issue_id], |row| {
                let ts: i64 = row.get(5)?;
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    field: row.get(2)?,
                    old_value: row.get(3)?,
                    new_value: row.get(4)?,
                    changed_at: Self::ts_to_dt(ts),
                })
            })?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IssueType;

    fn new_task(title: &str) -> NewIssue {
        NewIssue {
            title: title.to_string(),
            description: String::new(),
            issue_type: IssueType::Task,
            priority: Priority::Medium,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        }
    }

    fn new_issue(title: &str, desc: &str, itype: IssueType, priority: Priority) -> NewIssue {
        NewIssue {
            title: title.to_string(),
            description: desc.to_string(),
            issue_type: itype,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        }
    }

    // ── CRUD tests ───────────────────────────────────────────────────────

    #[test]
    fn test_create_returns_issue_with_generated_id() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Test issue")).unwrap();
        assert!(issue.id.starts_with("th-"), "ID should start with 'th-': {}", issue.id);
        assert_eq!(issue.id.len(), 9); // "th-" + 6 hex chars
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.status, IssueStatus::Open);
    }

    #[test]
    fn test_get_by_id() {
        let store = IssueStore::open_in_memory().unwrap();
        let created = store.create(&new_task("Find me")).unwrap();
        let fetched = store.get(&created.id).unwrap().expect("should find issue");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.title, "Find me");
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let store = IssueStore::open_in_memory().unwrap();
        let result = store.get("th-000000").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_all_issues() {
        let store = IssueStore::open_in_memory().unwrap();
        store.create(&new_task("A")).unwrap();
        store.create(&new_task("B")).unwrap();
        store.create(&new_task("C")).unwrap();

        let all = store.list(&IssueQuery::new()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_list_filtered_by_status() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Open one")).unwrap();
        store.create(&new_task("Open two")).unwrap();
        store.close(&[&a.id]).unwrap();

        let open = store.list(&IssueQuery::new().with_status(IssueStatus::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].title, "Open two");

        let closed = store.list(&IssueQuery::new().with_status(IssueStatus::Closed)).unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].title, "Open one");
    }

    #[test]
    fn test_list_filtered_by_priority() {
        let store = IssueStore::open_in_memory().unwrap();
        store.create(&new_issue("Critical", "", IssueType::Bug, Priority::Critical)).unwrap();
        store.create(&new_issue("Backlog", "", IssueType::Task, Priority::Backlog)).unwrap();

        let critical = store.list(&IssueQuery::new().with_priority(Priority::Critical)).unwrap();
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].title, "Critical");
    }

    #[test]
    fn test_update_changes_fields_and_records_history() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Original title")).unwrap();

        let updated = store
            .update(
                &issue.id,
                &IssueUpdate {
                    title: Some("New title".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.title, "New title");

        let history = store.get_history(&issue.id).unwrap();
        assert!(!history.is_empty());
        assert_eq!(history[0].field, "title");
        assert_eq!(history[0].old_value.as_deref(), Some("Original title"));
        assert_eq!(history[0].new_value.as_deref(), Some("New title"));
    }

    #[test]
    fn test_close_sets_status_and_closed_at() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Close me")).unwrap();
        assert!(issue.closed_at.is_none());

        let count = store.close(&[&issue.id]).unwrap();
        assert_eq!(count, 1);

        let closed = store.get(&issue.id).unwrap().unwrap();
        assert_eq!(closed.status, IssueStatus::Closed);
        assert!(closed.closed_at.is_some());
    }

    #[test]
    fn test_reopen_clears_closed_status() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Reopen me")).unwrap();
        store.close(&[&issue.id]).unwrap();

        let reopened = store.reopen(&issue.id).unwrap();
        assert_eq!(reopened.status, IssueStatus::Open);
        assert!(reopened.closed_at.is_none());
    }

    #[test]
    fn test_delete_removes_issue() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Delete me")).unwrap();
        store.delete(&issue.id).unwrap();
        assert!(store.get(&issue.id).unwrap().is_none());
    }

    // ── Dependency tests ─────────────────────────────────────────────────

    #[test]
    fn test_add_dep_creates_blocking_relationship() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();

        store.add_dep(&a.id, &b.id).unwrap();
        let deps = store.get_deps(&a.id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, b.id);
        assert_eq!(deps[0].dep_type, DepType::Blocks);
    }

    #[test]
    fn test_get_blockers_returns_blocking_issues() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();

        let blockers = store.get_blockers(&a.id).unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].id, b.id);
    }

    #[test]
    fn test_ready_excludes_issues_with_open_blockers() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Ready")).unwrap();
        let b = store.create(&new_task("Blocked")).unwrap();
        let c = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&b.id, &c.id).unwrap();

        let ready = store.ready().unwrap();
        let ready_ids: Vec<&str> = ready.iter().map(|i| i.id.as_str()).collect();
        assert!(ready_ids.contains(&a.id.as_str()), "Ready issue should be in list");
        assert!(!ready_ids.contains(&b.id.as_str()), "Blocked issue should NOT be in ready list");
        // c is a blocker but has no deps itself, so it IS ready
        assert!(ready_ids.contains(&c.id.as_str()), "Blocker issue should be ready (it has no blockers)");
    }

    #[test]
    fn test_blocked_returns_issues_with_open_blockers() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();

        let blocked = store.blocked().unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, a.id);

        // Close the blocker — now nothing should be blocked
        store.close(&[&b.id]).unwrap();
        let blocked = store.blocked().unwrap();
        assert!(blocked.is_empty());
    }

    // ── Labels & Comments ────────────────────────────────────────────────

    #[test]
    fn test_add_label_and_query_by_label() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("Labeled")).unwrap();
        store.create(&new_task("No label")).unwrap();

        store.add_label(&a.id, "backend").unwrap();

        let labeled = store.list(&IssueQuery::new().with_label("backend")).unwrap();
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].id, a.id);
        assert!(labeled[0].labels.contains(&"backend".to_string()));
    }

    #[test]
    fn test_add_comment_and_get_comments() {
        let store = IssueStore::open_in_memory().unwrap();
        let issue = store.create(&new_task("Commented")).unwrap();

        let c1 = store.add_comment(&issue.id, "First comment").unwrap();
        let c2 = store.add_comment(&issue.id, "Second comment").unwrap();

        assert!(c1.id.starts_with("th-"));
        assert_eq!(c1.content, "First comment");

        let comments = store.get_comments(&issue.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].content, "First comment");
        assert_eq!(comments[1].content, "Second comment");
        assert_eq!(comments[1].id, c2.id);
    }

    // ── Search ───────────────────────────────────────────────────────────

    #[test]
    fn test_search_finds_by_title_substring() {
        let store = IssueStore::open_in_memory().unwrap();
        store
            .create(&new_issue("Fix login bug", "auth related", IssueType::Bug, Priority::High))
            .unwrap();
        store
            .create(&new_issue("Add dashboard", "new feature", IssueType::Feature, Priority::Medium))
            .unwrap();

        let results = store.search("login").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fix login bug");

        // Also finds by description
        let results = store.search("new feature").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Add dashboard");
    }

    // ── Stats ────────────────────────────────────────────────────────────

    #[test]
    fn test_stats_returns_correct_counts() {
        let store = IssueStore::open_in_memory().unwrap();
        let a = store.create(&new_task("One")).unwrap();
        store.create(&new_task("Two")).unwrap();
        store.create(&new_task("Three")).unwrap();
        store.close(&[&a.id]).unwrap();

        let b = store.create(&new_task("Four")).unwrap();
        store
            .update(
                &b.id,
                &IssueUpdate {
                    status: Some(IssueStatus::InProgress),
                    ..Default::default()
                },
            )
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.open, 2);
        assert_eq!(stats.in_progress, 1);
        assert_eq!(stats.closed, 1);
        assert_eq!(stats.deferred, 0);
        assert_eq!(stats.total, 4);
    }
}
