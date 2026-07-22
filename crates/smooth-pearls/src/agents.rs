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

use crate::dolt::sql_escape;

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

    /// Rename an agent handle, carrying its mail with it. Used when a
    /// session boots under an auto-generated placeholder (`cc-<repo>-<sid>`)
    /// and later renames itself to something task-meaningful. Rewrites the
    /// roster row *and* every message addressed to/from `old`, so the
    /// renamed session keeps its inbox and thread history.
    ///
    /// # Errors
    /// Returns an error if `new` is empty, if `old` isn't registered, if an
    /// agent already exists under `new` (would merge two identities), or if
    /// a Dolt write fails.
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let old = old.trim();
        let new = new.trim();
        if new.is_empty() {
            anyhow::bail!("new agent name must not be empty");
        }
        if old == new {
            return Ok(());
        }
        if self.get(old)?.is_none() {
            anyhow::bail!("agent '{old}' is not registered");
        }
        if self.get(new)?.is_some() {
            anyhow::bail!("agent '{new}' already exists — pick a different handle");
        }
        let (old_e, new_e) = (sql_escape(old), sql_escape(new));
        self.dolt
            .exec(&format!(
                "UPDATE agents SET name = '{new_e}', last_seen = NOW(), status = 'online' WHERE name = '{old_e}'"
            ))
            .context("rename agent row")?;
        // Carry the mail so the renamed handle keeps its inbox + sent history.
        self.dolt
            .exec(&format!("UPDATE messages SET to_agent = '{new_e}' WHERE to_agent = '{old_e}'"))
            .context("rename inbound mail")?;
        self.dolt
            .exec(&format!("UPDATE messages SET from_agent = '{new_e}' WHERE from_agent = '{old_e}'"))
            .context("rename outbound mail")?;
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

    #[test]
    fn rename_moves_row_and_carries_mail() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        let mb = crate::Mailbox::new(s.dolt().clone());
        reg.register("cc-smooth-a21c", "claude-code", Some(7)).unwrap();
        reg.register("peer", "shell", None).unwrap();
        // Mail both directions across the placeholder handle.
        mb.send("peer", "cc-smooth-a21c", "hi there", None).unwrap();
        mb.send("cc-smooth-a21c", "peer", "hi back", None).unwrap();

        reg.rename("cc-smooth-a21c", "fix-auth").unwrap();

        // Roster row moved, old handle gone, harness/pid preserved.
        assert!(reg.get("cc-smooth-a21c").unwrap().is_none());
        let got = reg.get("fix-auth").unwrap().expect("renamed present");
        assert_eq!(got.harness, "claude-code");
        assert_eq!(got.pid, Some(7));
        assert_eq!(got.status, "online");

        // Inbox follows the rename.
        let inbox = mb.inbox("fix-auth", false, 50).unwrap();
        assert_eq!(inbox.len(), 1, "inbound mail should re-address to the new handle");
        assert_eq!(inbox[0].body, "hi there");
        let peer_inbox = mb.inbox("peer", false, 50).unwrap();
        assert_eq!(peer_inbox.len(), 1);
        assert_eq!(peer_inbox[0].from_agent, "fix-auth", "outbound mail should show the new sender");
    }

    #[test]
    fn rename_to_existing_handle_rejected() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("a", "shell", None).unwrap();
        reg.register("b", "shell", None).unwrap();
        assert!(reg.rename("a", "b").is_err(), "must not merge two identities");
        // Both untouched.
        assert!(reg.get("a").unwrap().is_some());
        assert!(reg.get("b").unwrap().is_some());
    }

    #[test]
    fn rename_unknown_source_rejected() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        assert!(reg.rename("ghost", "whatever").is_err());
    }

    #[test]
    fn rename_empty_target_rejected() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("a", "shell", None).unwrap();
        assert!(reg.rename("a", "   ").is_err());
    }

    #[test]
    fn rename_noop_when_same() {
        let (_t, s) = store();
        let reg = AgentRegistry::new(s.dolt().clone());
        reg.register("a", "shell", None).unwrap();
        reg.rename("a", "a").unwrap();
        assert!(reg.get("a").unwrap().is_some());
    }
}
