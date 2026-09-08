//! SQLite-backed pearl store.
//!
//! Pearl th-d3e842. One machine-global database (`~/.smooth/pearls.db`,
//! `$SMOOTH_PEARLS_DB` overrides) holds every project's pearls; each
//! row carries a `project` column = the canonical project root. The
//! root is resolved from any cwd as the **main** checkout even inside a
//! linked git worktree, which is what fixes "pearls created in
//! worktrees vanish". Not a git repo → the directory itself.
//!
//! Timestamps are UTC RFC3339 text with fixed microsecond width, so
//! lexical `<=` in SQL is chronological; comparisons always use a
//! Rust `Utc::now()` literal, never SQLite's `now`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::query::PearlQuery;
use crate::types::{
    NewPearl, Pearl, PearlComment, PearlDepType, PearlDependency, PearlHistoryEntry, PearlStats, PearlStatus, PearlType, PearlUpdate, Priority,
};

/// What [`PearlStore::import_pearl`] did with a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    Inserted,
    Updated,
    Unchanged,
}

/// Thread-safe SQLite-backed pearl store, scoped to one project.
#[derive(Clone)]
pub struct PearlStore {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
    project_root: PathBuf,
    project: String,
}

/// Generate a short ID: "th-" + first 6 hex chars of a UUID v4.
fn generate_id() -> String {
    let uuid = Uuid::new_v4();
    let hex = uuid.simple().to_string();
    format!("th-{}", &hex[..6])
}

/// Where the pearl database lives: `$SMOOTH_PEARLS_DB`, else `~/.smooth/pearls.db`.
#[must_use]
pub fn default_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SMOOTH_PEARLS_DB") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth").join("pearls.db")
}

/// Canonical project root for `start`.
///
/// The MAIN repository checkout when `start` is inside a git repo (linked
/// worktrees included — we take the parent of `--git-common-dir`), else
/// `start` itself. Never fails.
///
/// ponytail: a submodule's common dir is `<super>/.git/modules/<x>`, so it
/// would resolve to the wrong parent; nobody tracks pearls in a submodule.
#[must_use]
pub fn resolve_project_root(start: &Path) -> PathBuf {
    let fallback = || start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
    else {
        return fallback();
    };
    if !out.status.success() {
        return fallback();
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let common = Path::new(&common);
    common
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(fallback, |p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
}

/// Fixed-width UTC RFC3339 (`2026-09-07T21:23:00.123456Z`) — lexically ordered.
pub(crate) fn fmt_ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn now_ts() -> String {
    fmt_ts(Utc::now())
}

/// Naive timestamp shapes the pre-SQLite store used; accepted on read so
/// migrated rows parse like fresh ones.
const LEGACY_TS_FORMATS: &[&str] = &["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];

/// Parse a stored timestamp: RFC3339 (what we write) or a legacy naive form.
pub(crate) fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    LEGACY_TS_FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .map(|n| n.and_utc())
}

fn ts_or_now(s: &str) -> DateTime<Utc> {
    parse_ts(s).unwrap_or_else(Utc::now)
}

/// Column list every pearl SELECT uses, so `row_to_pearl` indexes are stable.
const PEARL_COLS: &str =
    "p.id, p.title, p.description, p.status, p.priority, p.pearl_type, p.parent_id, p.assigned_to, p.created_at, p.updated_at, p.closed_at, p.scheduled_at";

fn row_to_pearl(row: &Row<'_>) -> rusqlite::Result<Pearl> {
    let status: String = row.get(3)?;
    let priority: i64 = row.get(4)?;
    let pearl_type: String = row.get(5)?;
    Ok(Pearl {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        status: PearlStatus::from_str_loose(&status).unwrap_or(PearlStatus::Open),
        priority: u8::try_from(priority).ok().and_then(Priority::from_u8).unwrap_or(Priority::Medium),
        pearl_type: PearlType::from_str_loose(&pearl_type).unwrap_or(PearlType::Task),
        labels: Vec::new(), // filled by attach_labels
        parent_id: row.get(6)?,
        assigned_to: row.get(7)?,
        created_at: ts_or_now(&row.get::<_, String>(8)?),
        updated_at: ts_or_now(&row.get::<_, String>(9)?),
        closed_at: row.get::<_, Option<String>>(10)?.as_deref().and_then(parse_ts),
        scheduled_at: row.get::<_, Option<String>>(11)?.as_deref().and_then(parse_ts),
    })
}

impl PearlStore {
    /// Open the store for the project containing `project_root` (any path
    /// inside the repo works — see [`resolve_project_root`]). Creates the
    /// database and schema on first use and registers the project in
    /// `~/.smooth/registry.json`.
    pub fn open(project_root: &Path) -> Result<Self> {
        let root = resolve_project_root(project_root);
        let store = Self::open_with_db(&default_db_path(), &root)?;
        // Best-effort registry update; never fails the open.
        let _ = crate::registry::auto_register(&store.project_root);
        Ok(store)
    }

    /// Alias of [`Self::open`] — the database is created on demand, so
    /// "init" is just "open + register". Kept for callers and docs that
    /// spell it `th pearls init`.
    pub fn init(project_root: &Path) -> Result<Self> {
        Self::open(project_root)
    }

    /// Open the store at an explicit database file for `project_root`
    /// (taken verbatim, not git-resolved). Does NOT touch the global
    /// registry — this is the constructor tests use so they never write
    /// to `~/.smooth/`.
    pub fn open_with_db(db_path: &Path, project_root: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(db_path).with_context(|| format!("open pearl db {}", db_path.display()))?;
        // WAL so readers never block writers; the busy timeout turns
        // concurrent agents into a queue instead of "database is locked".
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
        conn.busy_timeout(std::time::Duration::from_secs(10)).context("set busy_timeout")?;
        Self::ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: db_path.to_path_buf(),
            project_root: project_root.to_path_buf(),
            project: project_root.to_string_lossy().into_owned(),
        })
    }

    fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pearls (
                 project      TEXT NOT NULL,
                 id           TEXT NOT NULL,
                 title        TEXT NOT NULL,
                 description  TEXT NOT NULL DEFAULT '',
                 status       TEXT NOT NULL DEFAULT 'open',
                 priority     INTEGER NOT NULL DEFAULT 2,
                 pearl_type   TEXT NOT NULL DEFAULT 'task',
                 parent_id    TEXT,
                 assigned_to  TEXT,
                 created_at   TEXT NOT NULL,
                 updated_at   TEXT NOT NULL,
                 closed_at    TEXT,
                 scheduled_at TEXT,
                 PRIMARY KEY (project, id)
             );
             CREATE INDEX IF NOT EXISTS pearls_project_status_idx ON pearls(project, status);
             CREATE TABLE IF NOT EXISTS pearl_dependencies (
                 project    TEXT NOT NULL,
                 pearl_id   TEXT NOT NULL,
                 depends_on TEXT NOT NULL,
                 dep_type   TEXT NOT NULL DEFAULT 'blocks',
                 PRIMARY KEY (project, pearl_id, depends_on)
             );
             CREATE TABLE IF NOT EXISTS pearl_labels (
                 project  TEXT NOT NULL,
                 pearl_id TEXT NOT NULL,
                 label    TEXT NOT NULL,
                 PRIMARY KEY (project, pearl_id, label)
             );
             CREATE TABLE IF NOT EXISTS pearl_comments (
                 seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                 project    TEXT NOT NULL,
                 id         TEXT NOT NULL,
                 pearl_id   TEXT NOT NULL,
                 content    TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE (project, id)
             );
             CREATE INDEX IF NOT EXISTS pearl_comments_pearl_idx ON pearl_comments(project, pearl_id);
             CREATE TABLE IF NOT EXISTS pearl_history (
                 seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                 project    TEXT NOT NULL,
                 id         TEXT NOT NULL,
                 pearl_id   TEXT NOT NULL,
                 field_name TEXT NOT NULL,
                 old_value  TEXT,
                 new_value  TEXT,
                 changed_at TEXT NOT NULL,
                 UNIQUE (project, id)
             );
             CREATE INDEX IF NOT EXISTS pearl_history_pearl_idx ON pearl_history(project, pearl_id);
             CREATE TABLE IF NOT EXISTS memories (
                 seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                 project    TEXT NOT NULL,
                 id         TEXT NOT NULL,
                 content    TEXT NOT NULL,
                 source     TEXT NOT NULL DEFAULT '',
                 created_at TEXT NOT NULL,
                 UNIQUE (project, id)
             );
             CREATE TABLE IF NOT EXISTS config (
                 project    TEXT NOT NULL,
                 k          TEXT NOT NULL,
                 v          TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (project, k)
             );
             CREATE TABLE IF NOT EXISTS sync_map (
                 project           TEXT NOT NULL,
                 pearl_id          TEXT NOT NULL,
                 remote_id         TEXT NOT NULL,
                 remote_updated_at TEXT NOT NULL,
                 local_updated_at  TEXT NOT NULL,
                 last_synced_at    TEXT NOT NULL,
                 PRIMARY KEY (project, pearl_id),
                 UNIQUE (project, remote_id)
             );",
        )
        .context("apply pearl schema")?;
        // SQLite has no `ADD COLUMN IF NOT EXISTS`; columns added after the
        // first release heal here, gated on `column_exists` (a bare ALTER
        // errors "duplicate column" on an already-migrated db).
        for (table, column, ddl) in Self::COLUMN_HEALS {
            if !Self::column_exists(conn, table, column)? {
                conn.execute(ddl, []).with_context(|| format!("add {table}.{column}"))?;
            }
        }
        Ok(())
    }

    /// (table, column, ddl) for columns added after the initial schema.
    const COLUMN_HEALS: &'static [(&'static str, &'static str, &'static str)] = &[];

    /// True when `table.column` exists, per `PRAGMA table_info`.
    fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for n in names {
            if n? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The canonical project root this store is scoped to.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The `project` column value (the root as a string).
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Path of the SQLite file backing this store.
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// A [`crate::MemoryStore`] over the same project + connection.
    #[must_use]
    pub fn memory(&self) -> crate::memory::MemoryStore {
        crate::memory::MemoryStore::new(self.clone())
    }

    /// Fresh pearl id unused in this project.
    fn fresh_id(&self, conn: &Connection) -> Result<String> {
        loop {
            let id = generate_id();
            let taken: Option<i64> = conn
                .query_row("SELECT 1 FROM pearls WHERE project = ?1 AND id = ?2", params![self.project, id], |r| r.get(0))
                .optional()?;
            if taken.is_none() {
                return Ok(id);
            }
        }
    }

    /// Populate `.labels` for a whole batch of pearls in ONE query (the
    /// per-pearl path was an N+1 that dominated `th prime` latency).
    fn attach_labels(&self, conn: &Connection, mut pearls: Vec<Pearl>) -> Result<Vec<Pearl>> {
        if pearls.is_empty() {
            return Ok(pearls);
        }
        let placeholders = std::iter::repeat_n("?", pearls.len()).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT pearl_id, label FROM pearl_labels WHERE project = ? AND pearl_id IN ({placeholders}) ORDER BY label");
        let mut stmt = conn.prepare(&sql)?;
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&self.project];
        for p in &pearls {
            args.push(&p.id);
        }
        let rows = stmt.query_map(args.as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut by_id: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for r in rows {
            let (pid, label) = r?;
            by_id.entry(pid).or_default().push(label);
        }
        for p in &mut pearls {
            if let Some(labels) = by_id.remove(&p.id) {
                p.labels = labels;
            }
        }
        Ok(pearls)
    }

    fn query_pearls(&self, conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Pearl>> {
        let mut stmt = conn.prepare(sql)?;
        let pearls = stmt.query_map(args, row_to_pearl)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_labels(conn, pearls)
    }

    fn get_in(&self, conn: &Connection, id: &str) -> Result<Option<Pearl>> {
        let mut found = self.query_pearls(
            conn,
            &format!("SELECT {PEARL_COLS} FROM pearls p WHERE p.project = ?1 AND p.id = ?2"),
            &[&self.project, &id],
        )?;
        Ok(found.pop())
    }

    // ── CRUD ────────────────────────────────────────────────────────────

    /// Create a new pearl.
    pub fn create(&self, new: &NewPearl) -> Result<Pearl> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let id = self.fresh_id(&tx)?;
        let now = now_ts();
        tx.execute(
            "INSERT INTO pearls (project, id, title, description, status, priority, pearl_type, assigned_to, parent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                self.project,
                id,
                new.title,
                new.description,
                PearlStatus::Open.as_str(),
                i64::from(new.priority.as_u8()),
                new.pearl_type.as_str(),
                new.assigned_to,
                new.parent_id,
                now,
            ],
        )?;
        for label in &new.labels {
            tx.execute(
                "INSERT OR IGNORE INTO pearl_labels (project, pearl_id, label) VALUES (?1, ?2, ?3)",
                params![self.project, id, label],
            )?;
        }
        let pearl = self.get_in(&tx, &id)?.ok_or_else(|| anyhow::anyhow!("pearl not found after create: {id}"))?;
        tx.commit()?;
        Ok(pearl)
    }

    /// Get a pearl by ID.
    pub fn get(&self, id: &str) -> Result<Option<Pearl>> {
        let conn = self.conn();
        self.get_in(&conn, id)
    }

    /// List pearls matching the given query.
    pub fn list(&self, query: &PearlQuery) -> Result<Vec<Pearl>> {
        let mut sql = format!("SELECT {PEARL_COLS} FROM pearls p");
        let mut conditions: Vec<String> = vec!["p.project = ?".into()];
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(self.project.clone())];

        if query.label.is_some() {
            sql.push_str(" JOIN pearl_labels l ON l.project = p.project AND l.pearl_id = p.id");
        }
        if let Some(ref status) = query.status {
            conditions.push("p.status = ?".into());
            args.push(Box::new(status.as_str()));
        }
        if let Some(ref priority) = query.priority {
            conditions.push("p.priority = ?".into());
            args.push(Box::new(i64::from(priority.as_u8())));
        }
        if let Some(ref pearl_type) = query.pearl_type {
            conditions.push("p.pearl_type = ?".into());
            args.push(Box::new(pearl_type.as_str()));
        }
        if let Some(ref label) = query.label {
            conditions.push("l.label = ?".into());
            args.push(Box::new(label.clone()));
        }
        if let Some(ref assigned_to) = query.assigned_to {
            conditions.push("p.assigned_to = ?".into());
            args.push(Box::new(assigned_to.clone()));
        }
        if let Some(ref parent_id) = query.parent_id {
            conditions.push("p.parent_id = ?".into());
            args.push(Box::new(parent_id.clone()));
        }
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
        // `limit == 0` is the "no limit" sentinel (web UI needs every pearl);
        // non-zero caps the result so LLM tool calls don't blow their context.
        sql.push_str(" ORDER BY p.priority ASC, p.created_at DESC");
        if query.limit > 0 {
            use std::fmt::Write as _;
            let _ = write!(sql, " LIMIT {}", query.limit);
        }
        let conn = self.conn();
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        self.query_pearls(&conn, &sql, &refs)
    }

    /// Update a pearl with partial changes. Records history for each changed field.
    pub fn update(&self, id: &str, updates: &PearlUpdate) -> Result<Pearl> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let current = self.get_in(&tx, id)?.ok_or_else(|| anyhow::anyhow!("pearl not found: {id}"))?;
        let now = now_ts();
        let set = |tx: &Connection, column: &str, val: &dyn rusqlite::ToSql| -> Result<()> {
            tx.execute(
                &format!("UPDATE pearls SET {column} = ?4, updated_at = ?3 WHERE project = ?1 AND id = ?2"),
                params![self.project, id, now, val],
            )?;
            Ok(())
        };

        if let Some(ref title) = updates.title {
            if *title != current.title {
                set(&tx, "title", title)?;
                self.record_history(&tx, id, "title", Some(&current.title), Some(title))?;
            }
        }
        if let Some(ref desc) = updates.description {
            if *desc != current.description {
                set(&tx, "description", desc)?;
                self.record_history(&tx, id, "description", Some(&current.description), Some(desc))?;
            }
        }
        if let Some(ref status) = updates.status {
            if *status != current.status {
                let closed_at = if *status == PearlStatus::Closed { Some(now.clone()) } else { None };
                tx.execute(
                    "UPDATE pearls SET status = ?4, closed_at = ?5, updated_at = ?3 WHERE project = ?1 AND id = ?2",
                    params![self.project, id, now, status.as_str(), closed_at],
                )?;
                self.record_history(&tx, id, "status", Some(current.status.as_str()), Some(status.as_str()))?;
            }
        }
        if let Some(ref priority) = updates.priority {
            if *priority != current.priority {
                set(&tx, "priority", &i64::from(priority.as_u8()))?;
                self.record_history(
                    &tx,
                    id,
                    "priority",
                    Some(&current.priority.as_u8().to_string()),
                    Some(&priority.as_u8().to_string()),
                )?;
            }
        }
        if let Some(ref pearl_type) = updates.pearl_type {
            if *pearl_type != current.pearl_type {
                set(&tx, "pearl_type", &pearl_type.as_str())?;
                self.record_history(&tx, id, "pearl_type", Some(current.pearl_type.as_str()), Some(pearl_type.as_str()))?;
            }
        }
        if let Some(ref assigned) = updates.assigned_to {
            set(&tx, "assigned_to", assigned)?;
        }
        if let Some(ref parent) = updates.parent_id {
            set(&tx, "parent_id", parent)?;
        }
        if let Some(ref scheduled) = updates.scheduled_at {
            let new = scheduled.map(fmt_ts);
            set(&tx, "scheduled_at", &new)?;
            let old = current.scheduled_at.map(fmt_ts);
            self.record_history(&tx, id, "scheduled_at", old.as_deref(), new.as_deref())?;
        }

        let pearl = self.get_in(&tx, id)?.ok_or_else(|| anyhow::anyhow!("pearl disappeared after update"))?;
        tx.commit()?;
        Ok(pearl)
    }

    /// Close one or more pearls. Returns the number actually closed.
    pub fn close(&self, ids: &[&str]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = now_ts();
        let mut count = 0;
        for id in ids {
            let changed = tx.execute(
                "UPDATE pearls SET status = 'closed', closed_at = ?3, updated_at = ?3 WHERE project = ?1 AND id = ?2 AND status != 'closed'",
                params![self.project, id, now],
            )?;
            if changed > 0 {
                self.record_history(&tx, id, "status", Some("open"), Some("closed"))?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// Reopen a closed pearl.
    pub fn reopen(&self, id: &str) -> Result<Pearl> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE pearls SET status = 'open', closed_at = NULL, updated_at = ?3 WHERE project = ?1 AND id = ?2",
            params![self.project, id, now_ts()],
        )?;
        self.record_history(&tx, id, "status", Some("closed"), Some("open"))?;
        let pearl = self.get_in(&tx, id)?.ok_or_else(|| anyhow::anyhow!("pearl not found: {id}"))?;
        tx.commit()?;
        Ok(pearl)
    }

    /// Delete a pearl entirely (labels, comments, deps, history included).
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let p = params![self.project, id];
        tx.execute("DELETE FROM pearl_labels WHERE project = ?1 AND pearl_id = ?2", p)?;
        tx.execute("DELETE FROM pearl_comments WHERE project = ?1 AND pearl_id = ?2", p)?;
        tx.execute("DELETE FROM pearl_dependencies WHERE project = ?1 AND (pearl_id = ?2 OR depends_on = ?2)", p)?;
        tx.execute("DELETE FROM pearl_history WHERE project = ?1 AND pearl_id = ?2", p)?;
        tx.execute("DELETE FROM pearls WHERE project = ?1 AND id = ?2", p)?;
        tx.commit()?;
        Ok(())
    }

    // ── History ─────────────────────────────────────────────────────────

    fn record_history(&self, conn: &Connection, pearl_id: &str, field: &str, old_value: Option<&str>, new_value: Option<&str>) -> Result<()> {
        conn.execute(
            "INSERT INTO pearl_history (project, id, pearl_id, field_name, old_value, new_value, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.project, generate_id(), pearl_id, field, old_value, new_value, now_ts()],
        )?;
        Ok(())
    }

    /// Get change history for a pearl, oldest first.
    pub fn get_history(&self, pearl_id: &str) -> Result<Vec<PearlHistoryEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, pearl_id, field_name, old_value, new_value, changed_at FROM pearl_history WHERE project = ?1 AND pearl_id = ?2 ORDER BY changed_at ASC, seq ASC",
        )?;
        let rows = stmt.query_map(params![self.project, pearl_id], |r| {
            Ok(PearlHistoryEntry {
                id: r.get(0)?,
                pearl_id: r.get(1)?,
                field: r.get(2)?,
                old_value: r.get(3)?,
                new_value: r.get(4)?,
                changed_at: ts_or_now(&r.get::<_, String>(5)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ── Dependencies ────────────────────────────────────────────────────

    /// Add a blocking dependency: `pearl_id` depends on `depends_on`. Idempotent.
    pub fn add_dep(&self, pearl_id: &str, depends_on: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO pearl_dependencies (project, pearl_id, depends_on, dep_type) VALUES (?1, ?2, ?3, ?4)",
            params![self.project, pearl_id, depends_on, PearlDepType::Blocks.as_str()],
        )?;
        Ok(())
    }

    /// Remove a dependency.
    pub fn remove_dep(&self, pearl_id: &str, depends_on: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM pearl_dependencies WHERE project = ?1 AND pearl_id = ?2 AND depends_on = ?3",
            params![self.project, pearl_id, depends_on],
        )?;
        Ok(())
    }

    /// Get all pearls that block the given pearl (unresolved blockers).
    pub fn get_blockers(&self, id: &str) -> Result<Vec<Pearl>> {
        let conn = self.conn();
        self.query_pearls(
            &conn,
            &format!(
                "SELECT {PEARL_COLS} FROM pearls p
                 JOIN pearl_dependencies d ON d.project = p.project AND d.depends_on = p.id
                 WHERE p.project = ?1 AND d.pearl_id = ?2 AND d.dep_type = 'blocks' AND p.status != 'closed'"
            ),
            &[&self.project, &id],
        )
    }

    /// Get all dependencies for a pearl.
    pub fn get_deps(&self, id: &str) -> Result<Vec<PearlDependency>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT pearl_id, depends_on, dep_type FROM pearl_dependencies WHERE project = ?1 AND pearl_id = ?2")?;
        let rows = stmt.query_map(params![self.project, id], |r| {
            let dep_type: String = r.get(2)?;
            Ok(PearlDependency {
                pearl_id: r.get(0)?,
                depends_on: r.get(1)?,
                dep_type: if dep_type == "related" { PearlDepType::Related } else { PearlDepType::Blocks },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every dependency row in this project (one query — `th pearls sync`
    /// reconciles the whole graph at once).
    pub fn all_deps(&self) -> Result<Vec<PearlDependency>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT pearl_id, depends_on, dep_type FROM pearl_dependencies WHERE project = ?1 ORDER BY pearl_id, depends_on")?;
        let rows = stmt.query_map(params![self.project], |r| {
            let dep_type: String = r.get(2)?;
            Ok(PearlDependency {
                pearl_id: r.get(0)?,
                depends_on: r.get(1)?,
                dep_type: if dep_type == "related" { PearlDepType::Related } else { PearlDepType::Blocks },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ── Labels ──────────────────────────────────────────────────────────

    /// Add a label to a pearl. Idempotent.
    pub fn add_label(&self, id: &str, label: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO pearl_labels (project, pearl_id, label) VALUES (?1, ?2, ?3)",
            params![self.project, id, label],
        )?;
        Ok(())
    }

    /// Remove a label from a pearl.
    pub fn remove_label(&self, id: &str, label: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM pearl_labels WHERE project = ?1 AND pearl_id = ?2 AND label = ?3",
            params![self.project, id, label],
        )?;
        Ok(())
    }

    // ── Comments ────────────────────────────────────────────────────────

    /// Add a comment to a pearl.
    pub fn add_comment(&self, pearl_id: &str, content: &str) -> Result<PearlComment> {
        let id = generate_id();
        let now = Utc::now();
        self.conn().execute(
            "INSERT INTO pearl_comments (project, id, pearl_id, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.project, id, pearl_id, content, fmt_ts(now)],
        )?;
        Ok(PearlComment {
            id,
            pearl_id: pearl_id.to_string(),
            content: content.to_string(),
            created_at: now,
        })
    }

    /// Get all comments for a pearl, ordered by creation time.
    pub fn get_comments(&self, pearl_id: &str) -> Result<Vec<PearlComment>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, pearl_id, content, created_at FROM pearl_comments WHERE project = ?1 AND pearl_id = ?2 ORDER BY created_at ASC, seq ASC")?;
        let rows = stmt.query_map(params![self.project, pearl_id], |r| {
            Ok(PearlComment {
                id: r.get(0)?,
                pearl_id: r.get(1)?,
                content: r.get(2)?,
                created_at: ts_or_now(&r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ── Query helpers ───────────────────────────────────────────────────

    /// Pearls that are open with no unresolved blocking dependencies.
    pub fn ready(&self) -> Result<Vec<Pearl>> {
        let conn = self.conn();
        self.query_pearls(
            &conn,
            &format!(
                "SELECT {PEARL_COLS} FROM pearls p
                 WHERE p.project = ?1 AND p.status = 'open'
                 AND NOT EXISTS (
                     SELECT 1 FROM pearl_dependencies d
                     JOIN pearls blocker ON blocker.project = d.project AND blocker.id = d.depends_on
                     WHERE d.project = p.project AND d.pearl_id = p.id AND d.dep_type = 'blocks' AND blocker.status != 'closed'
                 )
                 ORDER BY p.priority ASC, p.created_at DESC"
            ),
            &[&self.project],
        )
    }

    /// Scheduled pearls whose time has arrived (`scheduled_at <= now`, not
    /// closed), soonest-due first. Compared against a Rust UTC literal.
    pub fn due_scheduled(&self) -> Result<Vec<Pearl>> {
        let now = now_ts();
        let conn = self.conn();
        self.query_pearls(
            &conn,
            &format!(
                "SELECT {PEARL_COLS} FROM pearls p
                 WHERE p.project = ?1 AND p.scheduled_at IS NOT NULL AND p.scheduled_at <= ?2 AND p.status != 'closed'
                 ORDER BY p.scheduled_at ASC"
            ),
            &[&self.project, &now],
        )
    }

    /// Pearls that have unresolved blocking dependencies.
    pub fn blocked(&self) -> Result<Vec<Pearl>> {
        let conn = self.conn();
        self.query_pearls(
            &conn,
            &format!(
                "SELECT DISTINCT {PEARL_COLS} FROM pearls p
                 JOIN pearl_dependencies d ON d.project = p.project AND d.pearl_id = p.id
                 JOIN pearls blocker ON blocker.project = d.project AND blocker.id = d.depends_on
                 WHERE p.project = ?1 AND d.dep_type = 'blocks' AND blocker.status != 'closed' AND p.status != 'closed'
                 ORDER BY p.priority ASC"
            ),
            &[&self.project],
        )
    }

    /// Substring search on title and description (case-insensitive LIKE).
    pub fn search(&self, text: &str) -> Result<Vec<Pearl>> {
        let pattern = format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let conn = self.conn();
        self.query_pearls(
            &conn,
            &format!(
                "SELECT {PEARL_COLS} FROM pearls p
                 WHERE p.project = ?1 AND (p.title LIKE ?2 ESCAPE '\\' OR p.description LIKE ?2 ESCAPE '\\')
                 ORDER BY p.priority ASC, p.created_at DESC"
            ),
            &[&self.project, &pattern],
        )
    }

    // ── Config ──────────────────────────────────────────────────────────

    /// Read a per-project config value. `None` when the key is missing.
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT v FROM config WHERE project = ?1 AND k = ?2", params![self.project, key], |r| r.get(0))
            .optional()?)
    }

    /// Upsert a per-project config value.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO config (project, k, v, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project, k) DO UPDATE SET v = excluded.v, updated_at = excluded.updated_at",
            params![self.project, key, value, now_ts()],
        )?;
        Ok(())
    }

    /// List all config key/value pairs for this project.
    pub fn list_config(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT k, v FROM config WHERE project = ?1 ORDER BY k")?;
        let rows = stmt.query_map(params![self.project], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Aggregate stats across this project's pearls.
    pub fn stats(&self) -> Result<PearlStats> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM pearls WHERE project = ?1 GROUP BY status")?;
        let rows = stmt.query_map(params![self.project], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut stats = PearlStats::default();
        for row in rows {
            let (bucket, n) = row?;
            let count = usize::try_from(n).unwrap_or(0);
            match bucket.as_str() {
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

    // ── Raw import (migration) ──────────────────────────────────────────

    /// Import a pearl row verbatim, keeping its id and timestamps. Missing →
    /// inserted; present with an older `updated_at` → overwritten; otherwise
    /// left alone — so re-running an import is a no-op and edits made in
    /// the source after the first run still land. Used by store imports (sync).
    pub fn import_pearl(&self, pearl: &Pearl) -> Result<ImportOutcome> {
        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT updated_at FROM pearls WHERE project = ?1 AND id = ?2",
                params![self.project, pearl.id],
                |r| r.get(0),
            )
            .optional()?;
        let incoming = fmt_ts(pearl.updated_at);
        match existing {
            Some(current) if current >= incoming => return Ok(ImportOutcome::Unchanged),
            Some(_) => {
                conn.execute(
                    "UPDATE pearls SET title = ?3, description = ?4, status = ?5, priority = ?6, pearl_type = ?7, parent_id = ?8, assigned_to = ?9,
                     created_at = ?10, updated_at = ?11, closed_at = ?12, scheduled_at = ?13 WHERE project = ?1 AND id = ?2",
                    params![
                        self.project,
                        pearl.id,
                        pearl.title,
                        pearl.description,
                        pearl.status.as_str(),
                        i64::from(pearl.priority.as_u8()),
                        pearl.pearl_type.as_str(),
                        pearl.parent_id,
                        pearl.assigned_to,
                        fmt_ts(pearl.created_at),
                        incoming,
                        pearl.closed_at.map(fmt_ts),
                        pearl.scheduled_at.map(fmt_ts),
                    ],
                )?;
                return Ok(ImportOutcome::Updated);
            }
            None => {}
        }
        conn.execute(
            "INSERT INTO pearls (project, id, title, description, status, priority, pearl_type, parent_id, assigned_to, created_at, updated_at, closed_at, scheduled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                self.project,
                pearl.id,
                pearl.title,
                pearl.description,
                pearl.status.as_str(),
                i64::from(pearl.priority.as_u8()),
                pearl.pearl_type.as_str(),
                pearl.parent_id,
                pearl.assigned_to,
                fmt_ts(pearl.created_at),
                incoming,
                pearl.closed_at.map(fmt_ts),
                pearl.scheduled_at.map(fmt_ts),
            ],
        )?;
        Ok(ImportOutcome::Inserted)
    }

    /// Import a dependency row verbatim (idempotent).
    pub fn import_dep(&self, dep: &PearlDependency) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO pearl_dependencies (project, pearl_id, depends_on, dep_type) VALUES (?1, ?2, ?3, ?4)",
            params![self.project, dep.pearl_id, dep.depends_on, dep.dep_type.as_str()],
        )?;
        Ok(n > 0)
    }

    /// Import a label row verbatim (idempotent).
    pub fn import_label(&self, pearl_id: &str, label: &str) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO pearl_labels (project, pearl_id, label) VALUES (?1, ?2, ?3)",
            params![self.project, pearl_id, label],
        )?;
        Ok(n > 0)
    }

    /// Import a comment row verbatim, keeping its id + timestamp (idempotent).
    pub fn import_comment(&self, c: &PearlComment) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO pearl_comments (project, id, pearl_id, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.project, c.id, c.pearl_id, c.content, fmt_ts(c.created_at)],
        )?;
        Ok(n > 0)
    }

    /// Import a history row verbatim, keeping its id + timestamp (idempotent).
    pub fn import_history(&self, h: &PearlHistoryEntry) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO pearl_history (project, id, pearl_id, field_name, old_value, new_value, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.project, h.id, h.pearl_id, h.field, h.old_value, h.new_value, fmt_ts(h.changed_at)],
        )?;
        Ok(n > 0)
    }

    /// Mint an unused pearl id (`th-xxxxxx`) without inserting anything —
    /// `th pearls sync` pairs it with [`Self::import_pearl`] to materialize a
    /// remote work item locally.
    pub fn new_id(&self) -> Result<String> {
        let conn = self.conn();
        self.fresh_id(&conn)
    }

    /// Replace a pearl's label set wholesale (sync applies the remote set
    /// verbatim). Does not touch `updated_at`.
    pub fn replace_labels(&self, pearl_id: &str, labels: &[String]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM pearl_labels WHERE project = ?1 AND pearl_id = ?2", params![self.project, pearl_id])?;
        for label in labels {
            tx.execute(
                "INSERT OR IGNORE INTO pearl_labels (project, pearl_id, label) VALUES (?1, ?2, ?3)",
                params![self.project, pearl_id, label],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ── sync_map (th pearls sync, pearl th-19cca5) ──────────────────────

    /// The pearl ↔ remote work item mapping, or `None` when never synced.
    pub fn sync_map_get(&self, pearl_id: &str) -> Result<Option<SyncMapEntry>> {
        self.conn()
            .query_row(
                "SELECT pearl_id, remote_id, remote_updated_at, local_updated_at, last_synced_at FROM sync_map WHERE project = ?1 AND pearl_id = ?2",
                params![self.project, pearl_id],
                sync_map_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Reverse lookup by the remote work item id.
    pub fn sync_map_by_remote(&self, remote_id: &str) -> Result<Option<SyncMapEntry>> {
        self.conn()
            .query_row(
                "SELECT pearl_id, remote_id, remote_updated_at, local_updated_at, last_synced_at FROM sync_map WHERE project = ?1 AND remote_id = ?2",
                params![self.project, remote_id],
                sync_map_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every mapping for this project.
    pub fn sync_map_list(&self) -> Result<Vec<SyncMapEntry>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT pearl_id, remote_id, remote_updated_at, local_updated_at, last_synced_at FROM sync_map WHERE project = ?1 ORDER BY pearl_id")?;
        let rows = stmt.query_map(params![self.project], sync_map_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Insert or overwrite a mapping.
    pub fn sync_map_upsert(&self, e: &SyncMapEntry) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_map (project, pearl_id, remote_id, remote_updated_at, local_updated_at, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project, pearl_id) DO UPDATE SET remote_id = excluded.remote_id, remote_updated_at = excluded.remote_updated_at,
                 local_updated_at = excluded.local_updated_at, last_synced_at = excluded.last_synced_at",
            params![
                self.project,
                e.pearl_id,
                e.remote_id,
                fmt_ts(e.remote_updated_at),
                fmt_ts(e.local_updated_at),
                fmt_ts(e.last_synced_at)
            ],
        )?;
        Ok(())
    }

    /// Import a config row (idempotent — an existing key is left alone).
    pub fn import_config(&self, key: &str, value: &str, updated_at: DateTime<Utc>) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO config (project, k, v, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![self.project, key, value, fmt_ts(updated_at)],
        )?;
        Ok(n > 0)
    }
}

fn sync_map_row(row: &Row<'_>) -> rusqlite::Result<SyncMapEntry> {
    Ok(SyncMapEntry {
        pearl_id: row.get(0)?,
        remote_id: row.get(1)?,
        remote_updated_at: ts_or_now(&row.get::<_, String>(2)?),
        local_updated_at: ts_or_now(&row.get::<_, String>(3)?),
        last_synced_at: ts_or_now(&row.get::<_, String>(4)?),
    })
}

/// One row of `sync_map`: which remote work item a pearl is, and the
/// `updated_at` each side had when they were last reconciled (the
/// last-writer-wins baseline for `th pearls sync`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMapEntry {
    pub pearl_id: String,
    pub remote_id: String,
    pub remote_updated_at: DateTime<Utc>,
    pub local_updated_at: DateTime<Utc>,
    pub last_synced_at: DateTime<Utc>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests {
    use super::*;
    use crate::types::PearlType;

    /// A store on a private tempdir DB. The tempdir is leaked so the file
    /// outlives the returned store.
    pub fn test_store() -> PearlStore {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let store = PearlStore::open_with_db(&tmp.path().join("pearls.db"), &tmp.path().join("proj")).expect("open store");
        std::mem::forget(tmp);
        store
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
        let store = test_store();
        let pearl = store.create(&new_task("Test pearl")).unwrap();
        assert!(pearl.id.starts_with("th-"), "ID should start with 'th-': {}", pearl.id);
        assert_eq!(pearl.id.len(), 9);
        assert_eq!(pearl.title, "Test pearl");
        assert_eq!(pearl.status, PearlStatus::Open);
    }

    #[test]
    fn test_create_roundtrips_awkward_text() {
        // Regression for th-944230: `\'` in field text broke
        // the hand-rolled escape. Bound parameters make it a non-issue, but
        // the guard stays.
        let store = test_store();
        let title = r"backslash-quote \' title";
        let desc = "text with \\' backslash-quote, lone trailing \\, doubled \\\\, quote ', \n newline, unicode 世界 🦀, '; DROP TABLE pearls; --";
        let created = store.create(&new_pearl(title, desc, PearlType::Task, Priority::Medium)).unwrap();
        let fetched = store.get(&created.id).unwrap().expect("pearl should exist");
        assert_eq!(fetched.title, title);
        assert_eq!(fetched.description, desc);
    }

    #[test]
    fn test_create_with_labels() {
        let store = test_store();
        let mut new = new_task("labelled");
        new.labels = vec!["b".into(), "a".into(), "a".into()];
        let pearl = store.create(&new).unwrap();
        assert_eq!(pearl.labels, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_get_by_id() {
        let store = test_store();
        let created = store.create(&new_task("Find me")).unwrap();
        let fetched = store.get(&created.id).unwrap().expect("should find pearl");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.title, "Find me");
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let store = test_store();
        assert!(store.get("th-000000").unwrap().is_none());
    }

    #[test]
    fn test_list_all() {
        let store = test_store();
        store.create(&new_task("A")).unwrap();
        store.create(&new_task("B")).unwrap();
        store.create(&new_task("C")).unwrap();
        assert_eq!(store.list(&PearlQuery::new()).unwrap().len(), 3);
    }

    #[test]
    fn test_list_filtered_by_status() {
        let store = test_store();
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
        let store = test_store();
        for i in 0..150 {
            store.create(&new_task(&format!("p{i}"))).unwrap();
        }
        assert_eq!(store.list(&PearlQuery::new()).unwrap().len(), 100);
        assert_eq!(store.list(&PearlQuery::new().with_limit(0)).unwrap().len(), 150);
    }

    #[test]
    fn test_list_filtered_by_priority_type_assignee_parent() {
        let store = test_store();
        let root = store.create(&new_pearl("Critical", "", PearlType::Bug, Priority::Critical)).unwrap();
        let mut child = new_pearl("Backlog", "", PearlType::Task, Priority::Backlog);
        child.assigned_to = Some("alice".into());
        child.parent_id = Some(root.id.clone());
        store.create(&child).unwrap();

        let critical = store.list(&PearlQuery::new().with_priority(Priority::Critical)).unwrap();
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].title, "Critical");
        assert_eq!(store.list(&PearlQuery::new().with_type(PearlType::Bug)).unwrap().len(), 1);
        assert_eq!(store.list(&PearlQuery::new().with_assigned_to("alice")).unwrap().len(), 1);
        assert_eq!(store.list(&PearlQuery::new().with_parent(root.id)).unwrap().len(), 1);
    }

    #[test]
    fn test_update_changes_fields_and_records_history() {
        let store = test_store();
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
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].field, "title");
        assert_eq!(history[0].old_value.as_deref(), Some("Original title"));
        assert_eq!(history[0].new_value.as_deref(), Some("New title"));

        // Unchanged value → no history row.
        store
            .update(
                &pearl.id,
                &PearlUpdate {
                    title: Some("New title".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get_history(&pearl.id).unwrap().len(), 1);
    }

    #[test]
    fn test_update_missing_pearl_errors() {
        let store = test_store();
        assert!(store.update("th-nope00", &PearlUpdate::default()).is_err());
    }

    #[test]
    fn test_close_sets_status_and_closed_at() {
        let store = test_store();
        let pearl = store.create(&new_task("Close me")).unwrap();
        assert!(pearl.closed_at.is_none());

        assert_eq!(store.close(&[&pearl.id]).unwrap(), 1);
        let closed = store.get(&pearl.id).unwrap().unwrap();
        assert_eq!(closed.status, PearlStatus::Closed);
        assert!(closed.closed_at.is_some());
        // Idempotent: already-closed counts 0.
        assert_eq!(store.close(&[&pearl.id, "th-nope00"]).unwrap(), 0);
    }

    #[test]
    fn test_reopen_clears_closed_status() {
        let store = test_store();
        let pearl = store.create(&new_task("Reopen me")).unwrap();
        store.close(&[&pearl.id]).unwrap();
        let reopened = store.reopen(&pearl.id).unwrap();
        assert_eq!(reopened.status, PearlStatus::Open);
        assert!(reopened.closed_at.is_none());
    }

    #[test]
    fn test_delete_removes_pearl_and_children() {
        let store = test_store();
        let pearl = store.create(&new_task("Delete me")).unwrap();
        let other = store.create(&new_task("Other")).unwrap();
        store.add_label(&pearl.id, "x").unwrap();
        store.add_comment(&pearl.id, "c").unwrap();
        store.add_dep(&other.id, &pearl.id).unwrap();
        store.delete(&pearl.id).unwrap();
        assert!(store.get(&pearl.id).unwrap().is_none());
        assert!(store.get_comments(&pearl.id).unwrap().is_empty());
        assert!(store.get_deps(&other.id).unwrap().is_empty(), "deps pointing at the deleted pearl go too");
    }

    // ── Dependency tests ────────────────────────────────────────────────

    #[test]
    fn test_add_dep_creates_blocking_relationship() {
        let store = test_store();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();
        store.add_dep(&a.id, &b.id).unwrap(); // idempotent
        let deps = store.get_deps(&a.id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, b.id);
        assert_eq!(deps[0].dep_type, PearlDepType::Blocks);
        store.remove_dep(&a.id, &b.id).unwrap();
        assert!(store.get_deps(&a.id).unwrap().is_empty());
    }

    #[test]
    fn test_get_blockers_returns_blocking_pearls() {
        let store = test_store();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();
        let blockers = store.get_blockers(&a.id).unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].id, b.id);
    }

    #[test]
    fn test_ready_excludes_pearls_with_open_blockers() {
        let store = test_store();
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
        let store = test_store();
        let a = store.create(&new_task("Blocked")).unwrap();
        let b = store.create(&new_task("Blocker")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();

        let blocked = store.blocked().unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, a.id);

        store.close(&[&b.id]).unwrap();
        assert!(store.blocked().unwrap().is_empty());
    }

    // ── Labels & Comments ───────────────────────────────────────────────

    #[test]
    fn test_add_label_and_query_by_label() {
        let store = test_store();
        let a = store.create(&new_task("Labeled")).unwrap();
        store.create(&new_task("No label")).unwrap();
        store.add_label(&a.id, "backend").unwrap();

        let labeled = store.list(&PearlQuery::new().with_label("backend")).unwrap();
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].id, a.id);
        assert!(labeled[0].labels.contains(&"backend".to_string()));

        store.remove_label(&a.id, "backend").unwrap();
        assert!(store.list(&PearlQuery::new().with_label("backend")).unwrap().is_empty());
    }

    #[test]
    fn list_batch_loads_labels_per_pearl() {
        // Regression for th-2e1ad2: labels are batch-loaded in one query;
        // each pearl gets exactly its own, label-less pearls stay empty.
        let store = test_store();
        let a = store.create(&new_task("alpha")).unwrap();
        let b = store.create(&new_task("bravo")).unwrap();
        let c = store.create(&new_task("charlie")).unwrap();
        store.add_label(&a.id, "backend").unwrap();
        store.add_label(&a.id, "auth").unwrap();
        store.add_label(&b.id, "frontend").unwrap();

        let all = store.list(&PearlQuery::new()).unwrap();
        let get = |id: &str| all.iter().find(|p| p.id == id).unwrap().labels.clone();
        assert_eq!(get(&a.id), vec!["auth".to_string(), "backend".to_string()]);
        assert_eq!(get(&b.id), vec!["frontend".to_string()]);
        assert!(get(&c.id).is_empty());
    }

    #[test]
    fn test_add_comment_and_get_comments() {
        let store = test_store();
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
        let store = test_store();
        store
            .create(&new_pearl("Fix login bug", "auth related", PearlType::Bug, Priority::High))
            .unwrap();
        store
            .create(&new_pearl("Add dashboard", "new feature 100%", PearlType::Feature, Priority::Medium))
            .unwrap();

        let results = store.search("login").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fix login bug");

        let results = store.search("new feature").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Add dashboard");

        // LIKE metacharacters are literal.
        assert_eq!(store.search("100%").unwrap().len(), 1);
        assert!(store.search("_ogin").unwrap().is_empty());
    }

    // ── Stats / config ──────────────────────────────────────────────────

    #[test]
    fn test_stats_returns_correct_counts() {
        let store = test_store();
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

    #[test]
    fn test_config_round_trip() {
        let store = test_store();
        assert!(store.get_config("k").unwrap().is_none());
        store.set_config("k", "v1").unwrap();
        store.set_config("k", "v2").unwrap();
        store.set_config("a", "z").unwrap();
        assert_eq!(store.get_config("k").unwrap().as_deref(), Some("v2"));
        assert_eq!(
            store.list_config().unwrap(),
            vec![("a".to_string(), "z".to_string()), ("k".to_string(), "v2".to_string())]
        );
    }

    #[test]
    fn test_due_scheduled_uses_utc_literal() {
        let store = test_store();
        let past = store.create(&new_task("due already")).unwrap();
        let future = store.create(&new_task("due later")).unwrap();
        let unscheduled = store.create(&new_task("no schedule")).unwrap();

        let set = |id: &str, dt: DateTime<Utc>| {
            store
                .update(
                    id,
                    &PearlUpdate {
                        scheduled_at: Some(Some(dt)),
                        ..Default::default()
                    },
                )
                .unwrap();
        };
        set(&past.id, Utc::now() - chrono::Duration::hours(1));
        set(&future.id, Utc::now() + chrono::Duration::hours(1));

        let due = store.due_scheduled().unwrap();
        let ids: Vec<&str> = due.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&past.id.as_str()));
        assert!(!ids.contains(&future.id.as_str()));
        assert!(!ids.contains(&unscheduled.id.as_str()));
        assert_eq!(store.get_history(&past.id).unwrap()[0].field, "scheduled_at");

        store.close(&[&past.id]).unwrap();
        assert!(store.due_scheduled().unwrap().is_empty());
    }

    // ── Multi-project isolation ─────────────────────────────────────────

    #[test]
    fn projects_are_isolated_in_one_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("pearls.db");
        let a = PearlStore::open_with_db(&db, Path::new("/proj/a")).unwrap();
        let b = PearlStore::open_with_db(&db, Path::new("/proj/b")).unwrap();
        let pa = a.create(&new_task("in a")).unwrap();
        b.create(&new_task("in b")).unwrap();
        assert_eq!(a.list(&PearlQuery::new()).unwrap().len(), 1);
        assert_eq!(b.list(&PearlQuery::new()).unwrap().len(), 1);
        assert!(b.get(&pa.id).unwrap().is_none(), "ids are project-scoped");
        assert_eq!(a.stats().unwrap().total, 1);
        // Same id in two projects is allowed (pre-SQLite ids were per store).
        assert_eq!(b.import_pearl(&pa).unwrap(), ImportOutcome::Inserted);
        assert_eq!(b.import_pearl(&pa).unwrap(), ImportOutcome::Unchanged, "re-import is a no-op");
        assert_eq!(b.get(&pa.id).unwrap().unwrap().title, "in a");
        // A newer source row overwrites; an older one is ignored.
        let mut newer = pa.clone();
        newer.title = "renamed".into();
        newer.updated_at = pa.updated_at + chrono::Duration::seconds(1);
        assert_eq!(b.import_pearl(&newer).unwrap(), ImportOutcome::Updated);
        assert_eq!(b.get(&pa.id).unwrap().unwrap().title, "renamed");
        assert_eq!(b.import_pearl(&pa).unwrap(), ImportOutcome::Unchanged, "older row never clobbers");
        assert_eq!(b.get(&pa.id).unwrap().unwrap().title, "renamed");
    }

    /// Regression: a store opened from a linked git worktree must resolve
    /// to the SAME project as the main checkout, so pearls created in a
    /// worktree no longer vanish when the worktree is removed.
    #[test]
    fn worktree_resolves_to_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let out = std::process::Command::new("git").arg("-C").arg(cwd).args(args).output().expect("git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "--initial-branch=main"], &main);
        git(&["config", "user.email", "t@example.com"], &main);
        git(&["config", "user.name", "T"], &main);
        git(&["config", "commit.gpgsign", "false"], &main);
        std::fs::write(main.join("README"), "x").unwrap();
        git(&["add", "."], &main);
        git(&["commit", "-q", "--no-verify", "-m", "init"], &main);
        let wt = tmp.path().join("wt-feature");
        git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feature"], &main);
        let sub = wt.join("src");
        std::fs::create_dir_all(&sub).unwrap();

        let main_root = resolve_project_root(&main);
        assert_eq!(resolve_project_root(&wt), main_root);
        assert_eq!(resolve_project_root(&sub), main_root, "any subdir of a worktree resolves too");
        assert_eq!(main_root, main.canonicalize().unwrap());

        // Not a repo → the directory itself.
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(resolve_project_root(&plain), plain.canonicalize().unwrap());
    }

    #[test]
    fn default_db_path_points_at_pearls_db() {
        // The env override branch is exercised end-to-end by the CLI; no env
        // mutation here since tests share a process.
        assert!(default_db_path().ends_with("pearls.db"));
    }

    #[test]
    fn timestamps_round_trip_and_sort_lexically() {
        // Storage is microsecond-precision; Linux clocks carry nanoseconds,
        // so truncate before asserting the round trip.
        let a = parse_ts(&fmt_ts(Utc::now())).unwrap();
        let b = a + chrono::Duration::milliseconds(1);
        assert!(fmt_ts(a) < fmt_ts(b));
        assert_eq!(parse_ts(&fmt_ts(a)).unwrap(), a);
        // Pre-SQLite timestamp shapes still parse.
        assert!(parse_ts("2026-06-22 16:24:02").is_some());
        assert!(parse_ts("2026-06-22T16:24:02Z").is_some());
        assert!(parse_ts("").is_none());
    }

    #[test]
    fn sync_map_round_trips_and_reverse_looks_up() {
        let store = test_store();
        let p = store.create(&new_task("synced")).unwrap();
        assert!(store.sync_map_get(&p.id).unwrap().is_none());
        let t = Utc::now();
        let e = SyncMapEntry {
            pearl_id: p.id.clone(),
            remote_id: "11111111-2222-3333-4444-555555555555".into(),
            remote_updated_at: t,
            local_updated_at: p.updated_at,
            last_synced_at: t,
        };
        store.sync_map_upsert(&e).unwrap();
        let got = store.sync_map_get(&p.id).unwrap().unwrap();
        assert_eq!(got.remote_id, e.remote_id);
        assert_eq!(got.remote_updated_at.timestamp_millis(), t.timestamp_millis());
        assert_eq!(store.sync_map_by_remote(&e.remote_id).unwrap().unwrap().pearl_id, p.id);
        // Upsert overwrites in place.
        let later = t + chrono::Duration::seconds(5);
        store.sync_map_upsert(&SyncMapEntry { last_synced_at: later, ..e }).unwrap();
        assert_eq!(store.sync_map_list().unwrap().len(), 1);
        assert_eq!(store.sync_map_list().unwrap()[0].last_synced_at.timestamp_millis(), later.timestamp_millis());
    }

    #[test]
    fn all_deps_lists_every_edge() {
        let store = test_store();
        let a = store.create(&new_task("a")).unwrap();
        let b = store.create(&new_task("b")).unwrap();
        store.add_dep(&a.id, &b.id).unwrap();
        let deps = store.all_deps().unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!((deps[0].pearl_id.as_str(), deps[0].depends_on.as_str()), (a.id.as_str(), b.id.as_str()));
    }

    #[test]
    fn new_id_and_replace_labels() {
        let store = test_store();
        let id = store.new_id().unwrap();
        assert!(id.starts_with("th-") && store.get(&id).unwrap().is_none());
        let p = store.create(&new_task("labelled")).unwrap();
        store.replace_labels(&p.id, &["a".into(), "b".into()]).unwrap();
        assert_eq!(store.get(&p.id).unwrap().unwrap().labels, vec!["a", "b"]);
        store.replace_labels(&p.id, &["c".into()]).unwrap();
        assert_eq!(store.get(&p.id).unwrap().unwrap().labels, vec!["c"]);
    }

    #[test]
    fn column_exists_probe() {
        let store = test_store();
        let conn = store.conn();
        assert!(PearlStore::column_exists(&conn, "pearls", "title").unwrap());
        assert!(!PearlStore::column_exists(&conn, "pearls", "no_such_column").unwrap());
        assert!(!PearlStore::column_exists(&conn, "no_such_table", "x").unwrap());
    }

    #[test]
    fn concurrent_writers_all_succeed() {
        let store = test_store();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = store.clone();
                std::thread::spawn(move || {
                    for j in 0..5 {
                        s.create(&new_task(&format!("t{i}-{j}"))).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.stats().unwrap().total, 40);
    }
}
