//! SQLite database — schemas, migrations, CRUD.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::Connection;

/// Database handle (thread-safe via Mutex).
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Database {
    /// Open or create the database at the given path.
    pub fn open(path: &PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;

        // Enable WAL mode for concurrent read performance
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path.clone(),
        };

        db.ensure_schema()?;

        Ok(db)
    }

    /// Get the database file path.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Create all tables if they don't exist.
    fn ensure_schema(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                metadata TEXT DEFAULT '{}',
                created_at INTEGER DEFAULT (unixepoch()) NOT NULL,
                updated_at INTEGER DEFAULT (unixepoch()) NOT NULL
            );

            CREATE TABLE IF NOT EXISTS worker_runs (
                id TEXT PRIMARY KEY,
                bead_id TEXT NOT NULL,
                worker_id TEXT NOT NULL,
                sandbox_id TEXT,
                backend_type TEXT NOT NULL DEFAULT 'local-microsandbox',
                phase TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at INTEGER DEFAULT (unixepoch()) NOT NULL,
                completed_at INTEGER,
                metadata TEXT DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER DEFAULT (unixepoch()) NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_worker_runs_bead ON worker_runs(bead_id);
            CREATE INDEX IF NOT EXISTS idx_worker_runs_status ON worker_runs(status);
            CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);
            ",
        )?;

        Ok(())
    }

    /// Get a config value.
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        let mut stmt = conn.prepare("SELECT value FROM config WHERE key = ?1")?;
        let result = stmt.query_row([key], |row| row.get::<_, String>(0)).ok();
        Ok(result)
    }

    /// Set a config value (upsert).
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        conn.execute(
            "INSERT INTO config (key, value, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = unixepoch()",
            [key, value],
        )?;
        Ok(())
    }
}

/// Default database path: `~/.smooth/smooth.db`
pub fn default_db_path() -> PathBuf {
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth").join("smooth.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_open_in_memory() {
        let path = PathBuf::from(":memory:");
        let db = Database::open(&path);
        assert!(db.is_ok());
    }

    #[test]
    fn test_config_crud() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = Database::open(&path).unwrap();

        // Set and get
        db.set_config("test.key", "hello").unwrap();
        let val = db.get_config("test.key").unwrap();
        assert_eq!(val, Some("hello".to_string()));

        // Update
        db.set_config("test.key", "world").unwrap();
        let val = db.get_config("test.key").unwrap();
        assert_eq!(val, Some("world".to_string()));

        // Missing key
        let val = db.get_config("nonexistent").unwrap();
        assert_eq!(val, None);
    }
}
