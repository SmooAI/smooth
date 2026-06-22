//! Agent registry — the persistent, harness-agnostic roster of
//! agents that can send and receive messages.
//!
//! Pearl th-70aaef. Any process (Claude Code, opencode, pi, a shell
//! script, …) that runs `th agent register` lands a row in the
//! `agents` table keyed by its chosen `name`. Re-registering the same
//! name is idempotent — it just refreshes `last_seen`/`harness`/`pid`
//! and flips `status` back to `online`. Other agents discover who they
//! can message via [`AgentRegistry::list`].
//!
//! The table is created by `PearlStore::open`/`init`, and syncs via
//! `refs/dolt/data` like the rest of the pearl store, so agents in
//! otherwise-unconnected sessions/machines see each other after a
//! push/pull.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dolt::SmoothDolt;

/// A registered agent identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Unique, caller-chosen handle (e.g. `claude-web`, `pi-builder`).
    pub name: String,
    /// Harness/tool the agent runs under (`claude-code`, `opencode`,
    /// `pi`, `shell`, …). Empty when unknown.
    pub harness: String,
    /// OS process id of the registering process, if known.
    pub pid: Option<i64>,
    pub registered_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// `online` | `offline` (free-form; `online` on register/touch).
    pub status: String,
}

fn parse_datetime(value: &Value) -> DateTime<Utc> {
    // smooth-dolt returns `CURRENT_TIMESTAMP` defaults space-separated and
    // `NOW()` (used by `touch`) as RFC3339; the shared helper handles both.
    crate::messaging::parse_dolt_datetime(value.as_str().unwrap_or_default()).unwrap_or_else(Utc::now)
}

fn parse_agent(row: &Value) -> Agent {
    Agent {
        name: row["name"].as_str().unwrap_or_default().to_string(),
        harness: row["harness"].as_str().unwrap_or_default().to_string(),
        pid: row["pid"].as_i64(),
        registered_at: parse_datetime(&row["registered_at"]),
        last_seen: parse_datetime(&row["last_seen"]),
        status: row["status"].as_str().unwrap_or("online").to_string(),
    }
}

/// SQL-safe single-quote escape (smooth-dolt has no prepared statements).
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// API over the `agents` table. Cheap to clone.
#[derive(Clone)]
pub struct AgentRegistry {
    dolt: SmoothDolt,
}

impl AgentRegistry {
    /// Build a registry from an existing handle. The `agents` table is
    /// created by `PearlStore::open`/`init`.
    #[must_use]
    pub fn new(dolt: SmoothDolt) -> Self {
        Self { dolt }
    }

    /// Register (or refresh) an agent by name. Idempotent: re-registering
    /// the same name updates `harness`/`pid`/`last_seen` and sets
    /// `status = 'online'` rather than erroring.
    ///
    /// # Errors
    /// Returns an error if `name` is empty or the Dolt write fails.
    pub fn register(&self, name: &str, harness: &str, pid: Option<i64>) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("agent name must not be empty");
        }
        let pid_sql = pid.map_or_else(|| "NULL".to_string(), |p| p.to_string());
        let sql = format!(
            "INSERT INTO agents (name, harness, pid, status) VALUES ('{}', '{}', {}, 'online') \
             ON DUPLICATE KEY UPDATE harness = VALUES(harness), pid = VALUES(pid), last_seen = NOW(), status = 'online'",
            sql_escape(name),
            sql_escape(harness),
            pid_sql,
        );
        self.dolt.exec(&sql).context("register agent")?;
        Ok(())
    }

    /// Refresh an agent's `last_seen` (heartbeat) and mark it online.
    /// No-op if the agent isn't registered.
    ///
    /// # Errors
    /// Returns an error if the Dolt write fails.
    pub fn touch(&self, name: &str) -> Result<()> {
        let sql = format!("UPDATE agents SET last_seen = NOW(), status = 'online' WHERE name = '{}'", sql_escape(name));
        self.dolt.exec(&sql).context("touch agent")?;
        Ok(())
    }

    /// Mark an agent's status (e.g. `offline` on graceful shutdown).
    ///
    /// # Errors
    /// Returns an error if the Dolt write fails.
    pub fn set_status(&self, name: &str, status: &str) -> Result<()> {
        let sql = format!("UPDATE agents SET status = '{}' WHERE name = '{}'", sql_escape(status), sql_escape(name));
        self.dolt.exec(&sql).context("set agent status")?;
        Ok(())
    }

    /// List all registered agents, most-recently-seen first.
    ///
    /// # Errors
    /// Returns an error if the Dolt query fails.
    pub fn list(&self) -> Result<Vec<Agent>> {
        let rows = self
            .dolt
            .sql("SELECT name, harness, pid, registered_at, last_seen, status FROM agents ORDER BY last_seen DESC, name ASC")
            .context("list agents")?;
        Ok(rows.iter().map(parse_agent).collect())
    }

    /// Fetch a single agent by name.
    ///
    /// # Errors
    /// Returns an error if the Dolt query fails.
    pub fn get(&self, name: &str) -> Result<Option<Agent>> {
        let sql = format!(
            "SELECT name, harness, pid, registered_at, last_seen, status FROM agents WHERE name = '{}' LIMIT 1",
            sql_escape(name)
        );
        let rows = self.dolt.sql(&sql).context("get agent")?;
        Ok(rows.first().map(parse_agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::PearlStore;
    use tempfile::TempDir;

    fn store() -> (TempDir, PearlStore) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".smooth").join("dolt");
        let store = PearlStore::init(&dir).expect("init pearl store");
        (tmp, store)
    }

    #[test]
    fn register_then_list_and_get() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("claude-web", "claude-code", Some(4242)).unwrap();
        reg.register("pi-builder", "pi", None).unwrap();

        let all = reg.list().unwrap();
        assert_eq!(all.len(), 2);
        let names: Vec<_> = all.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"claude-web"));
        assert!(names.contains(&"pi-builder"));

        let got = reg.get("claude-web").unwrap().expect("present");
        assert_eq!(got.harness, "claude-code");
        assert_eq!(got.pid, Some(4242));
        assert_eq!(got.status, "online");
        assert_eq!(got.pid, Some(4242));
    }

    #[test]
    fn register_is_idempotent_upsert() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("dup", "claude-code", Some(1)).unwrap();
        reg.register("dup", "opencode", Some(2)).unwrap();
        let all = reg.list().unwrap();
        assert_eq!(all.len(), 1, "re-register same name must not duplicate");
        let got = reg.get("dup").unwrap().unwrap();
        assert_eq!(got.harness, "opencode");
        assert_eq!(got.pid, Some(2));
    }

    #[test]
    fn empty_name_rejected() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        assert!(reg.register("   ", "x", None).is_err());
    }

    #[test]
    fn set_status_offline() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("a", "shell", None).unwrap();
        reg.set_status("a", "offline").unwrap();
        assert_eq!(reg.get("a").unwrap().unwrap().status, "offline");
    }

    #[test]
    fn get_missing_returns_none() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        assert!(reg.get("nobody").unwrap().is_none());
    }
}
