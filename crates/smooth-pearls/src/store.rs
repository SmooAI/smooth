//! Dolt-backed pearl store.
//!
//! All pearl data lives in an embedded Dolt database (`.smooth/dolt/`),
//! accessed via the `smooth-dolt` Go binary subprocess. Queries return
//! JSON which we parse into Pearl structs. Mutations auto-commit.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::dolt::SmoothDolt;
use crate::query::PearlQuery;
use crate::types::{
    NewPearl, Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStats, PearlStatus, PearlType, PearlUpdate, Priority,
};

/// Thread-safe Dolt-backed pearl store.
#[derive(Clone)]
pub struct PearlStore {
    dolt: SmoothDolt,
}

/// Generate a short ID: "th-" + first 6 hex chars of a UUID v4.
fn generate_id() -> String {
    let uuid = Uuid::new_v4();
    let hex = uuid.simple().to_string();
    format!("th-{}", &hex[..6])
}

/// Escape a string for use in SQL string literals (single-quote escaping).
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

impl PearlStore {
    /// Open the pearl store at the given Dolt data directory.
    pub fn open(dolt_dir: &Path) -> Result<Self> {
        let dolt = SmoothDolt::new(dolt_dir)?;
        // Auto-register in global registry (best-effort)
        Self::auto_register_project(dolt_dir);
        Ok(Self { dolt })
    }

    /// Create a store with an explicit `SmoothDolt` handle (for testing).
    #[must_use]
    pub fn from_dolt(dolt: SmoothDolt) -> Self {
        Self { dolt }
    }

    /// Path to the Dolt data directory backing this store.
    #[must_use]
    pub fn dolt_path(&self) -> &Path {
        self.dolt.data_dir()
    }

    /// Initialize the Dolt database and create the pearl schema.
    pub fn init(dolt_dir: &Path) -> Result<Self> {
        let dolt = SmoothDolt::new(dolt_dir)?;
        dolt.init()?;
        Self::ensure_schema(&dolt)?;
        dolt.commit("initialize pearl schema")?;
        // Auto-register in global registry (best-effort)
        Self::auto_register_project(dolt_dir);
        Ok(Self { dolt })
    }

    /// Register this project in `~/.smooth/registry.json` (best-effort, never fails).
    fn auto_register_project(dolt_dir: &Path) {
        // dolt_dir is typically `.smooth/dolt/`, so project root is grandparent
        if let Some(project_root) = dolt_dir.parent().and_then(|p| p.parent()) {
            let _ = crate::registry::auto_register(project_root);
        }
    }

    /// Ensure all required tables exist.
    fn ensure_schema(dolt: &SmoothDolt) -> Result<()> {
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS pearls (
                id VARCHAR(20) PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                status VARCHAR(20) NOT NULL DEFAULT 'open',
                priority INT NOT NULL DEFAULT 2,
                pearl_type VARCHAR(20) NOT NULL DEFAULT 'task',
                parent_id VARCHAR(20),
                assigned_to VARCHAR(100),
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                closed_at DATETIME
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS pearl_dependencies (
                pearl_id VARCHAR(20) NOT NULL,
                depends_on VARCHAR(20) NOT NULL,
                dep_type VARCHAR(20) DEFAULT 'blocks',
                PRIMARY KEY (pearl_id, depends_on)
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS pearl_labels (
                pearl_id VARCHAR(20) NOT NULL,
                label VARCHAR(100) NOT NULL,
                PRIMARY KEY (pearl_id, label)
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS pearl_comments (
                id VARCHAR(20) PRIMARY KEY,
                pearl_id VARCHAR(20) NOT NULL,
                content TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS pearl_history (
                id VARCHAR(20) PRIMARY KEY,
                pearl_id VARCHAR(20) NOT NULL,
                field_name VARCHAR(50) NOT NULL,
                old_value TEXT,
                new_value TEXT,
                changed_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS sessions (
                id VARCHAR(40) PRIMARY KEY,
                title TEXT,
                model VARCHAR(100),
                started_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                ended_at DATETIME,
                message_count INT DEFAULT 0,
                token_count INT DEFAULT 0
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS session_messages (
                id VARCHAR(40) PRIMARY KEY,
                session_id VARCHAR(40) NOT NULL,
                from_actor VARCHAR(100) NOT NULL,
                to_actor VARCHAR(100) NOT NULL,
                content TEXT NOT NULL,
                message_type VARCHAR(20) NOT NULL DEFAULT 'Command',
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS orchestrator_snapshots (
                id VARCHAR(40) PRIMARY KEY,
                session_id VARCHAR(40) NOT NULL,
                bead_id VARCHAR(40) NOT NULL,
                phase VARCHAR(40) NOT NULL,
                operator_id VARCHAR(100) NOT NULL,
                dispatched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_checkpoint_id VARCHAR(40),
                status VARCHAR(20) NOT NULL DEFAULT 'Active'
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS memories (
                id VARCHAR(40) PRIMARY KEY,
                content TEXT NOT NULL,
                source VARCHAR(100),
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )?;
        dolt.exec(
            "CREATE TABLE IF NOT EXISTS config (
                k VARCHAR(255) PRIMARY KEY,
                v TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )?;
        Ok(())
    }

    /// Access the underlying `SmoothDolt` handle.
    #[must_use]
    pub fn dolt(&self) -> &SmoothDolt {
        &self.dolt
    }

    // ── JSON → Pearl parsing ────────────────────────────────────────────

    fn parse_pearl(row: &Value) -> Result<Pearl> {
        let id = row["id"].as_str().unwrap_or_default().to_string();
        let title = row["title"].as_str().unwrap_or_default().to_string();
        let description = row["description"].as_str().unwrap_or_default().to_string();
        let status_str = row["status"].as_str().unwrap_or("open");
        let priority_val = row["priority"].as_u64().unwrap_or(2) as u8;
        let type_str = row["pearl_type"].as_str().unwrap_or("task");
        let assigned_to = row["assigned_to"].as_str().map(String::from);
        let parent_id = row["parent_id"].as_str().map(String::from);
        let created_at = Self::parse_datetime(&row["created_at"]);
        let updated_at = Self::parse_datetime(&row["updated_at"]);
        let closed_at = if row["closed_at"].is_null() {
            None
        } else {
            Some(Self::parse_datetime(&row["closed_at"]))
        };

        Ok(Pearl {
            id,
            title,
            description,
            status: PearlStatus::from_str_loose(status_str).unwrap_or(PearlStatus::Open),
            priority: Priority::from_u8(priority_val).unwrap_or(Priority::Medium),
            pearl_type: PearlType::from_str_loose(type_str).unwrap_or(PearlType::Task),
            labels: Vec::new(), // filled after query
            assigned_to,
            parent_id,
            created_at,
            updated_at,
            closed_at,
        })
    }

    fn parse_datetime(val: &Value) -> DateTime<Utc> {
        if let Some(s) = val.as_str() {
            // Dolt returns datetimes as "2024-01-15 12:30:45" or ISO format
            if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                return ndt.and_utc();
            }
            if let Ok(dt) = s.parse::<DateTime<Utc>>() {
                return dt;
            }
        }
        Utc::now()
    }

    fn load_labels(&self, pearl_id: &str) -> Result<Vec<String>> {
        let rows = self.dolt.sql(&format!(
            "SELECT label FROM pearl_labels WHERE pearl_id = '{}' ORDER BY label",
            sql_escape(pearl_id)
        ))?;
        Ok(rows.iter().filter_map(|r| r["label"].as_str().map(String::from)).collect())
    }

    fn load_pearl_with_labels(&self, mut pearl: Pearl) -> Result<Pearl> {
        pearl.labels = self.load_labels(&pearl.id)?;
        Ok(pearl)
    }

    // ── Dolt version control ────────────────────────────────────────────

    /// View the Dolt commit log.
    pub fn dolt_log(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        self.dolt.log(limit)
    }

    /// Garbage collect the Dolt database.
    pub fn dolt_gc(&self) -> Result<()> {
        self.dolt.gc()?;
        Ok(())
    }

    // ── CRUD ────────────────────────────────────────────────────────────

    /// Create a new pearl.
    pub fn create(&self, new: &NewPearl) -> Result<Pearl> {
        let id = generate_id();
        let sql = format!(
            "INSERT INTO pearls (id, title, description, status, priority, pearl_type, assigned_to, parent_id, created_at, updated_at) \
             VALUES ('{}', '{}', '{}', '{}', {}, '{}', {}, {}, NOW(), NOW())",
            sql_escape(&id),
            sql_escape(&new.title),
            sql_escape(&new.description),
            PearlStatus::Open.as_str(),
            new.priority.as_u8(),
            new.pearl_type.as_str(),
            new.assigned_to.as_ref().map_or("NULL".to_string(), |a| format!("'{}'", sql_escape(a))),
            new.parent_id.as_ref().map_or("NULL".to_string(), |p| format!("'{}'", sql_escape(p))),
        );
        self.dolt.exec(&sql)?;

        for label in &new.labels {
            self.dolt.exec(&format!(
                "INSERT INTO pearl_labels (pearl_id, label) VALUES ('{}', '{}')",
                sql_escape(&id),
                sql_escape(label),
            ))?;
        }

        let pearl = self.get(&id)?.ok_or_else(|| anyhow::anyhow!("pearl not found after create: {id}"))?;
        self.dolt.commit(&format!("create pearl {id}: {}", new.title))?;
        Ok(pearl)
    }

    /// Get a pearl by ID.
    pub fn get(&self, id: &str) -> Result<Option<Pearl>> {
        let rows = self.dolt.sql(&format!("SELECT * FROM pearls WHERE id = '{}'", sql_escape(id)))?;
        match rows.first() {
            Some(row) => {
                let pearl = Self::parse_pearl(row)?;
                Ok(Some(self.load_pearl_with_labels(pearl)?))
            }
            None => Ok(None),
        }
    }

    /// List pearls matching the given query.
    pub fn list(&self, query: &PearlQuery) -> Result<Vec<Pearl>> {
        let mut sql = String::from("SELECT p.* FROM pearls p");
        let mut conditions: Vec<String> = Vec::new();

        if query.label.is_some() {
            sql.push_str(" JOIN pearl_labels l ON l.pearl_id = p.id");
        }

        if let Some(ref status) = query.status {
            conditions.push(format!("p.status = '{}'", status.as_str()));
        }
        if let Some(ref priority) = query.priority {
            conditions.push(format!("p.priority = {}", priority.as_u8()));
        }
        if let Some(ref pearl_type) = query.pearl_type {
            conditions.push(format!("p.pearl_type = '{}'", pearl_type.as_str()));
        }
        if let Some(ref label) = query.label {
            conditions.push(format!("l.label = '{}'", sql_escape(label)));
        }
        if let Some(ref assigned_to) = query.assigned_to {
            conditions.push(format!("p.assigned_to = '{}'", sql_escape(assigned_to)));
        }
        if let Some(ref parent_id) = query.parent_id {
            conditions.push(format!("p.parent_id = '{}'", sql_escape(parent_id)));
        }

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        // `limit == 0` is the "no limit" sentinel — useful for the web UI
        // which needs every pearl for counts, kanban columns, etc.
        // Non-zero values cap the result set so LLM tool calls don't
        // blow their context window.
        sql.push_str(" ORDER BY p.priority ASC, p.created_at DESC");
        if query.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", query.limit));
        }

        let rows = self.dolt.sql(&sql)?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let pearl = Self::parse_pearl(row)?;
            result.push(self.load_pearl_with_labels(pearl)?);
        }
        Ok(result)
    }

    /// Update a pearl with partial changes. Records history for each changed field.
    pub fn update(&self, id: &str, updates: &PearlUpdate) -> Result<Pearl> {
        let current = self.get(id)?.ok_or_else(|| anyhow::anyhow!("pearl not found: {id}"))?;

        if let Some(ref title) = updates.title {
            if *title != current.title {
                self.dolt.exec(&format!(
                    "UPDATE pearls SET title = '{}', updated_at = NOW() WHERE id = '{}'",
                    sql_escape(title),
                    sql_escape(id),
                ))?;
                self.record_history(id, "title", Some(&current.title), Some(title))?;
            }
        }

        if let Some(ref desc) = updates.description {
            if *desc != current.description {
                self.dolt.exec(&format!(
                    "UPDATE pearls SET description = '{}', updated_at = NOW() WHERE id = '{}'",
                    sql_escape(desc),
                    sql_escape(id),
                ))?;
                self.record_history(id, "description", Some(&current.description), Some(desc))?;
            }
        }

        if let Some(ref status) = updates.status {
            if *status != current.status {
                let closed_at = if *status == PearlStatus::Closed { "NOW()" } else { "NULL" };
                self.dolt.exec(&format!(
                    "UPDATE pearls SET status = '{}', updated_at = NOW(), closed_at = {} WHERE id = '{}'",
                    status.as_str(),
                    closed_at,
                    sql_escape(id),
                ))?;
                self.record_history(id, "status", Some(current.status.as_str()), Some(status.as_str()))?;
            }
        }

        if let Some(ref priority) = updates.priority {
            if *priority != current.priority {
                self.dolt.exec(&format!(
                    "UPDATE pearls SET priority = {}, updated_at = NOW() WHERE id = '{}'",
                    priority.as_u8(),
                    sql_escape(id),
                ))?;
                self.record_history(id, "priority", Some(&current.priority.as_u8().to_string()), Some(&priority.as_u8().to_string()))?;
            }
        }

        if let Some(ref pearl_type) = updates.pearl_type {
            if *pearl_type != current.pearl_type {
                self.dolt.exec(&format!(
                    "UPDATE pearls SET pearl_type = '{}', updated_at = NOW() WHERE id = '{}'",
                    pearl_type.as_str(),
                    sql_escape(id),
                ))?;
                self.record_history(id, "pearl_type", Some(current.pearl_type.as_str()), Some(pearl_type.as_str()))?;
            }
        }

        if let Some(ref assigned) = updates.assigned_to {
            let val = assigned.as_ref().map_or("NULL".to_string(), |a| format!("'{}'", sql_escape(a)));
            self.dolt.exec(&format!(
                "UPDATE pearls SET assigned_to = {val}, updated_at = NOW() WHERE id = '{}'",
                sql_escape(id)
            ))?;
        }

        if let Some(ref parent) = updates.parent_id {
            let val = parent.as_ref().map_or("NULL".to_string(), |p| format!("'{}'", sql_escape(p)));
            self.dolt.exec(&format!(
                "UPDATE pearls SET parent_id = {val}, updated_at = NOW() WHERE id = '{}'",
                sql_escape(id)
            ))?;
        }

        self.dolt.commit(&format!("update pearl {id}"))?;
        self.get(id)?.ok_or_else(|| anyhow::anyhow!("pearl disappeared after update"))
    }

    /// Close one or more pearls. Returns the number actually closed.
    pub fn close(&self, ids: &[&str]) -> Result<usize> {
        let mut count = 0;
        for id in ids {
            let rows = self
                .dolt
                .sql(&format!("SELECT id FROM pearls WHERE id = '{}' AND status != 'closed'", sql_escape(id),))?;
            if !rows.is_empty() {
                self.dolt.exec(&format!(
                    "UPDATE pearls SET status = 'closed', closed_at = NOW(), updated_at = NOW() WHERE id = '{}'",
                    sql_escape(id),
                ))?;
                self.record_history(id, "status", Some("open"), Some("closed"))?;
                count += 1;
            }
        }
        if count > 0 {
            let closed: Vec<_> = ids.iter().take(3).copied().collect();
            self.dolt.commit(&format!("close {} pearl(s): {}", count, closed.join(", ")))?;
        }
        Ok(count)
    }

    /// Reopen a closed pearl.
    pub fn reopen(&self, id: &str) -> Result<Pearl> {
        self.dolt.exec(&format!(
            "UPDATE pearls SET status = 'open', closed_at = NULL, updated_at = NOW() WHERE id = '{}'",
            sql_escape(id),
        ))?;
        self.record_history(id, "status", Some("closed"), Some("open"))?;
        self.dolt.commit(&format!("reopen pearl {id}"))?;
        self.get(id)?.ok_or_else(|| anyhow::anyhow!("pearl not found: {id}"))
    }

    /// Delete a pearl entirely.
    pub fn delete(&self, id: &str) -> Result<()> {
        // Delete child rows first (Dolt may not support CASCADE)
        self.dolt.exec(&format!("DELETE FROM pearl_labels WHERE pearl_id = '{}'", sql_escape(id)))?;
        self.dolt.exec(&format!("DELETE FROM pearl_comments WHERE pearl_id = '{}'", sql_escape(id)))?;
        self.dolt.exec(&format!(
            "DELETE FROM pearl_dependencies WHERE pearl_id = '{}' OR depends_on = '{}'",
            sql_escape(id),
            sql_escape(id)
        ))?;
        self.dolt.exec(&format!("DELETE FROM pearl_history WHERE pearl_id = '{}'", sql_escape(id)))?;
        self.dolt.exec(&format!("DELETE FROM pearls WHERE id = '{}'", sql_escape(id)))?;
        self.dolt.commit(&format!("delete pearl {id}"))?;
        Ok(())
    }

    // ── History ─────────────────────────────────────────────────────────

    fn record_history(&self, pearl_id: &str, field: &str, old_value: Option<&str>, new_value: Option<&str>) -> Result<()> {
        let hid = generate_id();
        let old_sql = old_value.map_or("NULL".to_string(), |v| format!("'{}'", sql_escape(v)));
        let new_sql = new_value.map_or("NULL".to_string(), |v| format!("'{}'", sql_escape(v)));
        self.dolt.exec(&format!(
            "INSERT INTO pearl_history (id, pearl_id, field_name, old_value, new_value, changed_at) VALUES ('{}', '{}', '{}', {}, {}, NOW())",
            sql_escape(&hid),
            sql_escape(pearl_id),
            sql_escape(field),
            old_sql,
            new_sql,
        ))?;
        Ok(())
    }

    /// Get change history for a pearl.
    pub fn get_history(&self, pearl_id: &str) -> Result<Vec<PearlHistoryEntry>> {
        let rows = self.dolt.sql(&format!(
            "SELECT id, pearl_id, field_name, old_value, new_value, changed_at FROM pearl_history WHERE pearl_id = '{}' ORDER BY changed_at ASC",
            sql_escape(pearl_id),
        ))?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            entries.push(PearlHistoryEntry {
                id: row["id"].as_str().unwrap_or_default().to_string(),
                pearl_id: row["pearl_id"].as_str().unwrap_or_default().to_string(),
                field: row["field_name"].as_str().unwrap_or_default().to_string(),
                old_value: row["old_value"].as_str().map(String::from),
                new_value: row["new_value"].as_str().map(String::from),
                changed_at: Self::parse_datetime(&row["changed_at"]),
            });
        }
        Ok(entries)
    }

    // ── Dependencies ────────────────────────────────────────────────────

    /// Add a blocking dependency: `pearl_id` depends on `depends_on`.
    pub fn add_dep(&self, pearl_id: &str, depends_on: &str) -> Result<()> {
        // Use REPLACE to handle "INSERT OR IGNORE" semantics
        self.dolt.exec(&format!(
            "REPLACE INTO pearl_dependencies (pearl_id, depends_on, dep_type) VALUES ('{}', '{}', '{}')",
            sql_escape(pearl_id),
            sql_escape(depends_on),
            PearlDepType::Blocks.as_str(),
        ))?;
        self.dolt.commit(&format!("add dep: {pearl_id} depends on {depends_on}"))?;
        Ok(())
    }

    /// Remove a dependency.
    pub fn remove_dep(&self, pearl_id: &str, depends_on: &str) -> Result<()> {
        self.dolt.exec(&format!(
            "DELETE FROM pearl_dependencies WHERE pearl_id = '{}' AND depends_on = '{}'",
            sql_escape(pearl_id),
            sql_escape(depends_on),
        ))?;
        self.dolt.commit(&format!("remove dep: {pearl_id} no longer depends on {depends_on}"))?;
        Ok(())
    }

    /// Get all pearls that block the given pearl (unresolved blockers).
    pub fn get_blockers(&self, id: &str) -> Result<Vec<Pearl>> {
        let rows = self.dolt.sql(&format!(
            "SELECT p.* FROM pearls p \
             JOIN pearl_dependencies d ON d.depends_on = p.id \
             WHERE d.pearl_id = '{}' AND d.dep_type = 'blocks' AND p.status != 'closed'",
            sql_escape(id),
        ))?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let pearl = Self::parse_pearl(row)?;
            result.push(self.load_pearl_with_labels(pearl)?);
        }
        Ok(result)
    }

    /// Get all dependencies for a pearl.
    pub fn get_deps(&self, id: &str) -> Result<Vec<PearlDependency>> {
        let rows = self.dolt.sql(&format!(
            "SELECT pearl_id, depends_on, dep_type FROM pearl_dependencies WHERE pearl_id = '{}'",
            sql_escape(id),
        ))?;
        let mut deps = Vec::with_capacity(rows.len());
        for row in &rows {
            let dep_type_str = row["dep_type"].as_str().unwrap_or("blocks");
            deps.push(PearlDependency {
                pearl_id: row["pearl_id"].as_str().unwrap_or_default().to_string(),
                depends_on: row["depends_on"].as_str().unwrap_or_default().to_string(),
                dep_type: if dep_type_str == "related" {
                    PearlDepType::Related
                } else {
                    PearlDepType::Blocks
                },
            });
        }
        Ok(deps)
    }

    // ── Labels ──────────────────────────────────────────────────────────

    /// Add a label to a pearl.
    pub fn add_label(&self, id: &str, label: &str) -> Result<()> {
        self.dolt.exec(&format!(
            "REPLACE INTO pearl_labels (pearl_id, label) VALUES ('{}', '{}')",
            sql_escape(id),
            sql_escape(label),
        ))?;
        self.dolt.commit(&format!("label {id}: +{label}"))?;
        Ok(())
    }

    /// Remove a label from a pearl.
    pub fn remove_label(&self, id: &str, label: &str) -> Result<()> {
        self.dolt.exec(&format!(
            "DELETE FROM pearl_labels WHERE pearl_id = '{}' AND label = '{}'",
            sql_escape(id),
            sql_escape(label),
        ))?;
        self.dolt.commit(&format!("label {id}: -{label}"))?;
        Ok(())
    }

    // ── Comments ────────────────────────────────────────────────────────

    /// Add a comment to a pearl.
    pub fn add_comment(&self, pearl_id: &str, content: &str) -> Result<PearlComment> {
        let id = generate_id();
        self.dolt.exec(&format!(
            "INSERT INTO pearl_comments (id, pearl_id, content, created_at) VALUES ('{}', '{}', '{}', NOW())",
            sql_escape(&id),
            sql_escape(pearl_id),
            sql_escape(content),
        ))?;
        let truncated = if content.len() > 60 { &content[..60] } else { content };
        self.dolt.commit(&format!("comment on {pearl_id}: {truncated}"))?;
        Ok(PearlComment {
            id,
            pearl_id: pearl_id.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
        })
    }

    /// Get all comments for a pearl, ordered by creation time.
    pub fn get_comments(&self, pearl_id: &str) -> Result<Vec<PearlComment>> {
        let rows = self.dolt.sql(&format!(
            "SELECT id, pearl_id, content, created_at FROM pearl_comments WHERE pearl_id = '{}' ORDER BY created_at ASC",
            sql_escape(pearl_id),
        ))?;
        let mut comments = Vec::with_capacity(rows.len());
        for row in &rows {
            comments.push(PearlComment {
                id: row["id"].as_str().unwrap_or_default().to_string(),
                pearl_id: row["pearl_id"].as_str().unwrap_or_default().to_string(),
                content: row["content"].as_str().unwrap_or_default().to_string(),
                created_at: Self::parse_datetime(&row["created_at"]),
            });
        }
        Ok(comments)
    }

    // ── Query helpers ───────────────────────────────────────────────────

    /// Pearls that are open with no unresolved blocking dependencies.
    pub fn ready(&self) -> Result<Vec<Pearl>> {
        let rows = self.dolt.sql(
            "SELECT p.* FROM pearls p \
             WHERE p.status = 'open' \
             AND NOT EXISTS ( \
                 SELECT 1 FROM pearl_dependencies d \
                 JOIN pearls blocker ON blocker.id = d.depends_on \
                 WHERE d.pearl_id = p.id AND d.dep_type = 'blocks' AND blocker.status != 'closed' \
             ) \
             ORDER BY p.priority ASC, p.created_at DESC",
        )?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let pearl = Self::parse_pearl(row)?;
            result.push(self.load_pearl_with_labels(pearl)?);
        }
        Ok(result)
    }

    /// Pearls that have unresolved blocking dependencies.
    pub fn blocked(&self) -> Result<Vec<Pearl>> {
        let rows = self.dolt.sql(
            "SELECT DISTINCT p.* FROM pearls p \
             JOIN pearl_dependencies d ON d.pearl_id = p.id \
             JOIN pearls blocker ON blocker.id = d.depends_on \
             WHERE d.dep_type = 'blocks' AND blocker.status != 'closed' AND p.status != 'closed' \
             ORDER BY p.priority ASC",
        )?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let pearl = Self::parse_pearl(row)?;
            result.push(self.load_pearl_with_labels(pearl)?);
        }
        Ok(result)
    }

    /// Full-text search on title and description (LIKE-based).
    pub fn search(&self, text: &str) -> Result<Vec<Pearl>> {
        let pattern = sql_escape(text);
        let rows = self.dolt.sql(&format!(
            "SELECT * FROM pearls WHERE title LIKE '%{pattern}%' OR description LIKE '%{pattern}%' ORDER BY priority ASC, created_at DESC",
        ))?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let pearl = Self::parse_pearl(row)?;
            result.push(self.load_pearl_with_labels(pearl)?);
        }
        Ok(result)
    }

    /// Aggregate stats across all pearls.
    /// Read a key/value from the Dolt `config` table. Returns `None`
    /// when the key is missing. This replaces the legacy SQLite
    /// `smooth.db::config` table — all config now lives in the same
    /// Dolt store as pearls, which means it's version-controlled and
    /// syncable across machines via `th pearls push/pull`.
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        let rows = self.dolt.sql(&format!("SELECT v FROM config WHERE k = '{}'", sql_escape(key)))?;
        Ok(rows.first().and_then(|row| row["v"].as_str().map(String::from)))
    }

    /// Upsert a key/value into the Dolt `config` table.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.dolt.exec(&format!(
            "INSERT INTO config (k, v, updated_at) VALUES ('{}', '{}', NOW()) \
             ON DUPLICATE KEY UPDATE v = '{}', updated_at = NOW()",
            sql_escape(key),
            sql_escape(value),
            sql_escape(value),
        ))?;
        self.dolt.commit(&format!("config: set {key}"))?;
        Ok(())
    }

    /// List all config key/value pairs.
    pub fn list_config(&self) -> Result<Vec<(String, String)>> {
        let rows = self.dolt.sql("SELECT k, v FROM config ORDER BY k")?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let k = row["k"].as_str()?.to_string();
                let v = row["v"].as_str()?.to_string();
                Some((k, v))
            })
            .collect())
    }

    pub fn stats(&self) -> Result<PearlStats> {
        let rows = self.dolt.sql("SELECT status, COUNT(*) as cnt FROM pearls GROUP BY status")?;
        let mut stats = PearlStats::default();
        for row in &rows {
            let status_val = row["status"].as_str().unwrap_or_default();
            #[allow(clippy::cast_possible_truncation)]
            let count = row["cnt"].as_u64().unwrap_or(0) as usize;
            match status_val {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PearlType;

    /// Create a test store in a temp directory. Requires smooth-dolt binary.
    fn test_store() -> Option<PearlStore> {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                // Leak the tempdir so it stays alive for the test
                std::mem::forget(tmp);
                Some(store)
            }
            Err(_) => {
                // smooth-dolt binary not available — skip test
                None
            }
        }
    }

    fn new_task(title: &str) -> NewPearl {
        NewPearl {
            title: title.to_string(),
            description: String::new(),
            pearl_type: PearlType::Task,
            priority: Priority::Medium,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        }
    }

    fn new_pearl(title: &str, desc: &str, ptype: PearlType, priority: Priority) -> NewPearl {
        NewPearl {
            title: title.to_string(),
            description: desc.to_string(),
            pearl_type: ptype,
            priority,
            assigned_to: None,
            parent_id: None,
            labels: Vec::new(),
        }
    }

    // ── CRUD tests ──────────────────────────────────────────────────────

    #[test]
    fn test_create_returns_pearl_with_generated_id() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Test pearl")).unwrap();
        assert!(pearl.id.starts_with("th-"), "ID should start with 'th-': {}", pearl.id);
        assert_eq!(pearl.id.len(), 9);
        assert_eq!(pearl.title, "Test pearl");
        assert_eq!(pearl.status, PearlStatus::Open);
    }

    #[test]
    fn test_get_by_id() {
        let Some(store) = test_store() else { return };
        let created = store.create(&new_task("Find me")).unwrap();
        let fetched = store.get(&created.id).unwrap().expect("should find pearl");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.title, "Find me");
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let Some(store) = test_store() else { return };
        let result = store.get("th-000000").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_all() {
        let Some(store) = test_store() else { return };
        store.create(&new_task("A")).unwrap();
        store.create(&new_task("B")).unwrap();
        store.create(&new_task("C")).unwrap();
        let all = store.list(&PearlQuery::new()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_list_filtered_by_status() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Open one")).unwrap();
        store.create(&new_task("Open two")).unwrap();
        store.close(&[&a.id]).unwrap();

        let open = store.list(&PearlQuery::new().with_status(PearlStatus::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].title, "Open two");

        let closed = store.list(&PearlQuery::new().with_status(PearlStatus::Closed)).unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].title, "Open one");
    }

    #[test]
    fn test_list_limit_zero_is_unbounded() {
        let Some(store) = test_store() else { return };
        // Create >100 pearls so the old default would have truncated.
        for i in 0..150 {
            store.create(&new_task(&format!("p{i}"))).unwrap();
        }

        // Default limit (100) caps the result.
        let capped = store.list(&PearlQuery::new()).unwrap();
        assert_eq!(capped.len(), 100);

        // limit == 0 returns all rows.
        let all = store.list(&PearlQuery::new().with_limit(0)).unwrap();
        assert_eq!(all.len(), 150);
    }

    #[test]
    fn test_list_filtered_by_priority() {
        let Some(store) = test_store() else { return };
        store.create(&new_pearl("Critical", "", PearlType::Bug, Priority::Critical)).unwrap();
        store.create(&new_pearl("Backlog", "", PearlType::Task, Priority::Backlog)).unwrap();

        let critical = store.list(&PearlQuery::new().with_priority(Priority::Critical)).unwrap();
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].title, "Critical");
    }

    #[test]
    fn test_update_changes_fields_and_records_history() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Original title")).unwrap();

        let updated = store
            .update(
                &pearl.id,
                &PearlUpdate {
                    title: Some("New title".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.title, "New title");

        let history = store.get_history(&pearl.id).unwrap();
        assert!(!history.is_empty());
        assert_eq!(history[0].field, "title");
        assert_eq!(history[0].old_value.as_deref(), Some("Original title"));
        assert_eq!(history[0].new_value.as_deref(), Some("New title"));
    }

    #[test]
    fn test_close_sets_status_and_closed_at() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Close me")).unwrap();
        assert!(pearl.closed_at.is_none());

        let count = store.close(&[&pearl.id]).unwrap();
        assert_eq!(count, 1);

        let closed = store.get(&pearl.id).unwrap().unwrap();
        assert_eq!(closed.status, PearlStatus::Closed);
        assert!(closed.closed_at.is_some());
    }

    #[test]
    fn test_reopen_clears_closed_status() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Reopen me")).unwrap();
        store.close(&[&pearl.id]).unwrap();

        let reopened = store.reopen(&pearl.id).unwrap();
        assert_eq!(reopened.status, PearlStatus::Open);
        assert!(reopened.closed_at.is_none());
    }

    #[test]
    fn test_delete_removes_pearl() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Delete me")).unwrap();
        store.delete(&pearl.id).unwrap();
        assert!(store.get(&pearl.id).unwrap().is_none());
    }

    // ── Dependency tests ────────────────────────────────────────────────

    #[test]
    fn test_add_dep_creates_blocking_relationship() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();

        store.add_dep(&a.id, &b.id).unwrap();
        let deps = store.get_deps(&a.id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, b.id);
        assert_eq!(deps[0].dep_type, PearlDepType::Blocks);
    }

    #[test]
    fn test_get_blockers_returns_blocking_pearls() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();

        let blockers = store.get_blockers(&a.id).unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].id, b.id);
    }

    #[test]
    fn test_ready_excludes_pearls_with_open_blockers() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Ready")).unwrap();
        let b = store.create(&new_task("Blocked")).unwrap();
        let c = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&b.id, &c.id).unwrap();

        let ready = store.ready().unwrap();
        let ready_ids: Vec<&str> = ready.iter().map(|p| p.id.as_str()).collect();
        assert!(ready_ids.contains(&a.id.as_str()));
        assert!(!ready_ids.contains(&b.id.as_str()));
        assert!(ready_ids.contains(&c.id.as_str()));
    }

    #[test]
    fn test_blocked_returns_pearls_with_open_blockers() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();

        let blocked = store.blocked().unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, a.id);

        store.close(&[&b.id]).unwrap();
        let blocked = store.blocked().unwrap();
        assert!(blocked.is_empty());
    }

    // ── Labels & Comments ───────────────────────────────────────────────

    #[test]
    fn test_add_label_and_query_by_label() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("Labeled")).unwrap();
        store.create(&new_task("No label")).unwrap();

        store.add_label(&a.id, "backend").unwrap();

        let labeled = store.list(&PearlQuery::new().with_label("backend")).unwrap();
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].id, a.id);
        assert!(labeled[0].labels.contains(&"backend".to_string()));
    }

    #[test]
    fn test_add_comment_and_get_comments() {
        let Some(store) = test_store() else { return };
        let pearl = store.create(&new_task("Commented")).unwrap();

        store.add_comment(&pearl.id, "First comment").unwrap();
        store.add_comment(&pearl.id, "Second comment").unwrap();

        let comments = store.get_comments(&pearl.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].content, "First comment");
        assert_eq!(comments[1].content, "Second comment");
    }

    // ── Search ──────────────────────────────────────────────────────────

    #[test]
    fn test_search_finds_by_title_substring() {
        let Some(store) = test_store() else { return };
        store
            .create(&new_pearl("Fix login bug", "auth related", PearlType::Bug, Priority::High))
            .unwrap();
        store
            .create(&new_pearl("Add dashboard", "new feature", PearlType::Feature, Priority::Medium))
            .unwrap();

        let results = store.search("login").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fix login bug");

        let results = store.search("new feature").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Add dashboard");
    }

    // ── Stats ───────────────────────────────────────────────────────────

    #[test]
    fn test_stats_returns_correct_counts() {
        let Some(store) = test_store() else { return };
        let a = store.create(&new_task("One")).unwrap();
        store.create(&new_task("Two")).unwrap();
        store.create(&new_task("Three")).unwrap();
        store.close(&[&a.id]).unwrap();

        let b = store.create(&new_task("Four")).unwrap();
        store
            .update(
                &b.id,
                &PearlUpdate {
                    status: Some(PearlStatus::InProgress),
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
