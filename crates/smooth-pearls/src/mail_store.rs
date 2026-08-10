//! Machine-level agent mail + presence — SQLite at `~/.smooth/mail.db`.
//!
//! Pearl th-374f85 (ADR-010). Agent mail used to live in the per-repo Dolt
//! pearl store, which made the mailbox a function of *where you were standing*:
//! `find_dolt_dir()` walked up from the cwd, so two agents in two worktrees of
//! the same repo had two different, unconnected mailboxes. Dolt is also
//! single-writer, so a handful of concurrent agents wedged the whole store with
//! `Error 1105: database is read only`, and every send paid a ~0.7s cold boot
//! plus a git push.
//!
//! Agents are a property of the **machine**, not of a checkout, so mail now
//! lives in one SQLite file per machine. Sends are instant local writes and
//! concurrent writers just queue on SQLite's lock.
//!
//! The old [`crate::Mailbox`] / [`crate::AgentRegistry`] are untouched so
//! existing per-repo data stays readable; nothing is migrated (mail is
//! ephemeral coordination chatter).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Recipient name used for broadcast messages.
pub const BROADCAST: &str = "all";

/// Presence state an agent publishes for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Registered, nothing in flight.
    #[default]
    Idle,
    /// Actively working a task.
    Working,
    /// Blocked on someone else (a human, another agent, CI).
    Waiting,
    /// Gone — set explicitly, or by the reaper when the pid is dead.
    Offline,
}

impl AgentStatus {
    /// The wire/storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Offline => "offline",
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            "waiting" => Ok(Self::Waiting),
            "offline" => Ok(Self::Offline),
            other => bail!("unknown agent status '{other}' (expected idle|working|waiting|offline)"),
        }
    }
}

/// What a message is *for*, so a receiving agent can triage without reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// FYI, no reply expected.
    #[default]
    Note,
    /// Asks the recipient to do or answer something.
    Request,
    /// Answers an earlier request.
    Result,
    /// Transfers ownership of work.
    Handoff,
    /// Asks the recipient to stop what it's doing.
    Cancel,
}

impl MessageKind {
    /// The wire/storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Request => "request",
            Self::Result => "result",
            Self::Handoff => "handoff",
            Self::Cancel => "cancel",
        }
    }
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "note" => Ok(Self::Note),
            "request" => Ok(Self::Request),
            "result" => Ok(Self::Result),
            "handoff" => Ok(Self::Handoff),
            "cancel" => Ok(Self::Cancel),
            other => bail!("unknown message type '{other}' (expected note|request|result|handoff|cancel)"),
        }
    }
}

/// A registered agent identity plus the context it registered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailAgent {
    /// Unique, caller-chosen handle (e.g. `fix-auth`, `pi-builder`).
    pub name: String,
    /// Harness/tool tag (`claude-code`, `opencode`, `shell`, …).
    pub harness: String,
    /// OS process id of the registering process, if known.
    pub pid: Option<i64>,
    /// Main repository directory the agent registered from.
    pub repo: Option<String>,
    /// Worktree root (differs from `repo` for linked worktrees).
    pub worktree: Option<String>,
    /// Checked-out branch at registration time.
    pub branch: Option<String>,
    /// Free-form "what I'm doing" published with `th agent status`.
    pub task: Option<String>,
    pub status: AgentStatus,
    pub registered_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// A single message. `read_at` is **per-recipient**: it is populated for the
/// agent whose inbox produced the row, and `None` everywhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessage {
    pub id: String,
    pub from_agent: String,
    /// Recipient agent name, or [`BROADCAST`] (`all`).
    pub to_agent: String,
    pub body: String,
    /// Root message id when this is a reply; `None` for a thread root.
    pub thread_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: MessageKind,
    /// Higher = more urgent. 0 is the default.
    pub priority: i64,
    pub created_at: DateTime<Utc>,
    /// Monotonic per-store insert order.
    pub seq: i64,
    /// When the querying agent acked it, if it has.
    pub read_at: Option<DateTime<Utc>>,
}

impl MailMessage {
    /// The id that identifies this message's thread (its own id when it is the
    /// root, else the root it replied under).
    #[must_use]
    pub fn thread_root(&self) -> &str {
        self.thread_id.as_deref().unwrap_or(&self.id)
    }
}

/// Where the mail database lives: `$SMOOTH_MAIL_DB`, else `~/.smooth/mail.db`.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SMOOTH_MAIL_DB") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth").join("mail.db")
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc))
}

fn parse_opt_dt(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(v).ok()).map(|d| d.with_timezone(&Utc))
}

fn generate_id() -> String {
    format!("msg-{}", &Uuid::new_v4().simple().to_string()[..8])
}

/// Is `pid` a live process on this machine?
///
/// ponytail: shells out to `kill -0` because `unsafe_code = "forbid"` rules out
/// calling `libc::kill` directly and no process crate is in the tree. One fork
/// per registered agent, only on `list()`. Swap for `nix`/`sysinfo` if the
/// roster ever gets big enough to notice.
fn pid_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    if cfg!(not(unix)) {
        // No cheap liveness check — assume alive rather than reap wrongly.
        return true;
    }
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `git rev-parse` context of `cwd`: (repo root, worktree root, branch).
/// All-`None` outside a repo or without git — never an error.
fn git_context(cwd: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let run = |args: &[&str]| -> Option<Vec<String>> {
        let out = std::process::Command::new("git").args(args).current_dir(cwd).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
        )
    };
    let paths = run(&["rev-parse", "--show-toplevel", "--path-format=absolute", "--git-common-dir"]).unwrap_or_default();
    let worktree = paths.first().cloned();
    // `--git-common-dir` points at the MAIN repo's `.git`, so its parent is the
    // main checkout even when we're standing in a linked worktree.
    let repo = paths
        .get(1)
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| worktree.clone());
    let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]).and_then(|v| v.into_iter().next());
    (repo, worktree, branch)
}

const AGENT_COLS: &str = "name, harness, pid, repo, worktree, branch, task, status, registered_at, last_seen";

fn row_to_agent(row: &Row<'_>) -> rusqlite::Result<MailAgent> {
    let status: String = row.get(7)?;
    Ok(MailAgent {
        name: row.get(0)?,
        harness: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        pid: row.get(2)?,
        repo: row.get(3)?,
        worktree: row.get(4)?,
        branch: row.get(5)?,
        task: row.get(6)?,
        status: status.parse().unwrap_or(AgentStatus::Idle),
        registered_at: parse_dt(&row.get::<_, String>(8)?),
        last_seen: parse_dt(&row.get::<_, String>(9)?),
    })
}

const MSG_COLS: &str = "m.id, m.from_agent, m.to_agent, m.body, m.thread_id, m.type, m.priority, m.created_at, m.seq";

fn row_to_message(row: &Row<'_>, read_at: Option<&str>) -> rusqlite::Result<MailMessage> {
    let kind: String = row.get(5)?;
    Ok(MailMessage {
        id: row.get(0)?,
        from_agent: row.get(1)?,
        to_agent: row.get(2)?,
        body: row.get(3)?,
        thread_id: row.get::<_, Option<String>>(4)?.filter(|s| !s.is_empty()),
        kind: kind.parse().unwrap_or(MessageKind::Note),
        priority: row.get(6)?,
        created_at: parse_dt(&row.get::<_, String>(7)?),
        seq: row.get(8)?,
        read_at: parse_opt_dt(read_at),
    })
}

/// The machine-level agent mail + presence store.
///
/// Not `Sync` (it owns a `rusqlite::Connection`) — open one per thread or
/// process; SQLite's own locking is what makes concurrent writers safe.
pub struct MailStore {
    conn: Connection,
}

impl MailStore {
    /// Open (creating if needed) the store at [`default_path`].
    ///
    /// # Errors
    /// Returns an error if the file can't be created or the schema can't be
    /// applied.
    pub fn open_default() -> Result<Self> {
        Self::open(&default_path())
    }

    /// Open (creating if needed) the store at `path`.
    ///
    /// # Errors
    /// Returns an error if the file can't be created or the schema can't be
    /// applied.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open mail db {}", path.display()))?;
        // WAL so a reader (a `th msg watch` poll) never blocks a writer; the
        // busy timeout is what turns concurrent sends into a queue instead of
        // an error. Both are best-effort: a filesystem that can't do WAL still
        // works, just with coarser locking.
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
        conn.busy_timeout(std::time::Duration::from_secs(10)).context("set busy_timeout")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                 name          TEXT PRIMARY KEY,
                 harness       TEXT,
                 pid           INTEGER,
                 repo          TEXT,
                 worktree      TEXT,
                 branch        TEXT,
                 task          TEXT,
                 status        TEXT NOT NULL DEFAULT 'idle',
                 registered_at TEXT NOT NULL,
                 last_seen     TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                 seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                 id         TEXT NOT NULL UNIQUE,
                 from_agent TEXT NOT NULL,
                 to_agent   TEXT NOT NULL,
                 body       TEXT NOT NULL,
                 thread_id  TEXT,
                 type       TEXT NOT NULL DEFAULT 'note',
                 priority   INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_to_idx ON messages(to_agent);
             CREATE INDEX IF NOT EXISTS messages_thread_idx ON messages(thread_id);
             CREATE TABLE IF NOT EXISTS message_reads (
                 message_id TEXT NOT NULL,
                 agent      TEXT NOT NULL,
                 read_at    TEXT NOT NULL,
                 PRIMARY KEY (message_id, agent)
             );",
        )
        .context("apply mail schema")?;
        Ok(Self { conn })
    }

    // ── Agents ─────────────────────────────────────────────────────

    /// Register (or resume) an agent by name. Idempotent: an existing handle
    /// keeps its mail, its `task`, and any non-`offline` status; only the
    /// harness/pid/git context and `last_seen` are refreshed.
    ///
    /// Git context (repo / worktree / branch) is sniffed from `cwd`,
    /// best-effort.
    ///
    /// # Errors
    /// Returns an error if `name` is empty or the write fails.
    pub fn register(&self, name: &str, harness: &str, pid: Option<i64>, cwd: &Path) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("agent name must not be empty");
        }
        let (repo, worktree, branch) = git_context(cwd);
        let ts = now();
        self.conn
            .execute(
                "INSERT INTO agents (name, harness, pid, repo, worktree, branch, status, registered_at, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?7)
                 ON CONFLICT(name) DO UPDATE SET
                     harness   = excluded.harness,
                     pid       = excluded.pid,
                     repo      = excluded.repo,
                     worktree  = excluded.worktree,
                     branch    = excluded.branch,
                     last_seen = excluded.last_seen,
                     status    = CASE WHEN agents.status = 'offline' THEN 'idle' ELSE agents.status END",
                params![name, harness.trim(), pid, repo, worktree, branch, ts],
            )
            .context("register agent")?;
        Ok(())
    }

    /// Refresh an agent's `last_seen` (heartbeat). No-op if unregistered.
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub fn touch(&self, name: &str) -> Result<()> {
        self.conn
            .execute("UPDATE agents SET last_seen = ?1 WHERE name = ?2", params![now(), name.trim()])
            .context("touch agent")?;
        Ok(())
    }

    /// Rename a handle, carrying its mail (inbound, outbound, and read state)
    /// with it.
    ///
    /// # Errors
    /// Returns an error if `new` is empty, `old` isn't registered, `new` is
    /// already taken, or a write fails.
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let (old, new) = (old.trim(), new.trim());
        if new.is_empty() {
            bail!("new agent name must not be empty");
        }
        if old == new {
            return Ok(());
        }
        if self.get_agent(old)?.is_none() {
            bail!("agent '{old}' is not registered");
        }
        if self.get_agent(new)?.is_some() {
            bail!("agent '{new}' already exists — pick a different handle");
        }
        let tx = self.conn.unchecked_transaction().context("begin rename")?;
        tx.execute("UPDATE agents SET name = ?1, last_seen = ?2 WHERE name = ?3", params![new, now(), old])?;
        tx.execute("UPDATE messages SET to_agent = ?1 WHERE to_agent = ?2", params![new, old])?;
        tx.execute("UPDATE messages SET from_agent = ?1 WHERE from_agent = ?2", params![new, old])?;
        tx.execute("UPDATE message_reads SET agent = ?1 WHERE agent = ?2", params![new, old])?;
        tx.commit().context("commit rename")?;
        Ok(())
    }

    /// Publish presence. `task` of `Some("")` clears the task; `None` leaves it.
    ///
    /// # Errors
    /// Returns an error if `name` isn't registered or the write fails.
    pub fn set_status(&self, name: &str, status: AgentStatus, task: Option<&str>) -> Result<()> {
        let name = name.trim();
        let changed = match task {
            Some(t) => {
                let t = t.trim();
                let t = if t.is_empty() { None } else { Some(t) };
                self.conn.execute(
                    "UPDATE agents SET status = ?1, task = ?2, last_seen = ?3 WHERE name = ?4",
                    params![status.as_str(), t, now(), name],
                )?
            }
            None => self.conn.execute(
                "UPDATE agents SET status = ?1, last_seen = ?2 WHERE name = ?3",
                params![status.as_str(), now(), name],
            )?,
        };
        if changed == 0 {
            bail!("agent '{name}' is not registered — run `th agent register` first");
        }
        Ok(())
    }

    /// Every registered agent, most-recently-seen first. Reaps first: any agent
    /// whose recorded pid is no longer alive on this machine is flipped to
    /// `offline`, so a crashed session stops looking online forever.
    ///
    /// # Errors
    /// Returns an error if a query or the reap write fails.
    pub fn list_agents(&self) -> Result<Vec<MailAgent>> {
        self.reap()?;
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {AGENT_COLS} FROM agents ORDER BY last_seen DESC, name ASC"))?;
        let rows = stmt.query_map([], row_to_agent)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Fetch one agent by name.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn get_agent(&self, name: &str) -> Result<Option<MailAgent>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {AGENT_COLS} FROM agents WHERE name = ?1"))?;
        Ok(stmt.query_row(params![name.trim()], row_to_agent).optional()?)
    }

    /// Mark agents with dead pids offline. Returns how many were reaped.
    ///
    /// # Errors
    /// Returns an error if a query or write fails.
    pub fn reap(&self) -> Result<usize> {
        let dead: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT name, pid FROM agents WHERE pid IS NOT NULL AND status != 'offline'")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().filter(|(_, pid)| !pid_alive(*pid)).map(|(name, _)| name).collect()
        };
        for name in &dead {
            self.conn.execute("UPDATE agents SET status = 'offline' WHERE name = ?1", params![name])?;
        }
        Ok(dead.len())
    }

    // ── Mail ───────────────────────────────────────────────────────

    /// Send a message. `to` is a recipient handle or [`BROADCAST`]. Returns the
    /// new message id.
    ///
    /// # Errors
    /// Returns an error if `from`/`to`/`body` are empty or the write fails.
    pub fn send(&self, from: &str, to: &str, body: &str, kind: MessageKind, priority: i64, thread_id: Option<&str>) -> Result<String> {
        let (from, to, body) = (from.trim(), to.trim(), body.trim());
        if from.is_empty() || to.is_empty() {
            bail!("message sender and recipient must not be empty");
        }
        if body.is_empty() {
            bail!("message body must not be empty");
        }
        let id = generate_id();
        self.conn
            .execute(
                "INSERT INTO messages (id, from_agent, to_agent, body, thread_id, type, priority, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, from, to, body, thread_id, kind.as_str(), priority, now()],
            )
            .context("insert message")?;
        Ok(id)
    }

    /// Messages addressed to `agent` (plus broadcasts), oldest first. Unread
    /// means "no ack row for **this** agent", so a broadcast is unread for
    /// everyone until each acks it individually.
    ///
    /// An agent never sees its own sends (otherwise every broadcast bounces
    /// straight back into the sender's watcher).
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn inbox(&self, agent: &str, unread_only: bool, limit: usize) -> Result<Vec<MailMessage>> {
        let agent = agent.trim();
        let unread = if unread_only { "AND r.read_at IS NULL" } else { "" };
        let sql = format!(
            "SELECT {MSG_COLS}, r.read_at
             FROM messages m
             LEFT JOIN message_reads r ON r.message_id = m.id AND r.agent = ?1
             WHERE (m.to_agent = ?1 OR m.to_agent = '{BROADCAST}') AND m.from_agent != ?1 {unread}
             ORDER BY m.priority DESC, m.seq ASC
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![agent, i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                let read_at = r.get::<_, Option<String>>(9)?;
                row_to_message(r, read_at.as_deref())
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Messages sent by `agent`, newest first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn sent(&self, agent: &str, limit: usize) -> Result<Vec<MailMessage>> {
        let sql = format!("SELECT {MSG_COLS} FROM messages m WHERE m.from_agent = ?1 ORDER BY m.seq DESC LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![agent.trim(), i64::try_from(limit).unwrap_or(i64::MAX)], |r| row_to_message(r, None))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Fetch one message by id. `read_at` is always `None` (it is per-recipient
    /// — use [`MailStore::inbox`] when you need it).
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn get_message(&self, id: &str) -> Result<Option<MailMessage>> {
        let sql = format!("SELECT {MSG_COLS} FROM messages m WHERE m.id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt.query_row(params![id.trim()], |r| row_to_message(r, None)).optional()?)
    }

    /// A thread (root plus replies), oldest first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn thread(&self, root_id: &str) -> Result<Vec<MailMessage>> {
        let sql = format!("SELECT {MSG_COLS} FROM messages m WHERE m.id = ?1 OR m.thread_id = ?1 ORDER BY m.seq ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![root_id.trim()], |r| row_to_message(r, None))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Acknowledge (mark read) one message for `agent`. Idempotent.
    ///
    /// # Errors
    /// Returns an error if the message doesn't exist or the write fails.
    pub fn ack(&self, agent: &str, message_id: &str) -> Result<()> {
        let message_id = message_id.trim();
        if self.get_message(message_id)?.is_none() {
            bail!("no message {message_id}");
        }
        self.conn
            .execute(
                "INSERT OR IGNORE INTO message_reads (message_id, agent, read_at) VALUES (?1, ?2, ?3)",
                params![message_id, agent.trim(), now()],
            )
            .context("ack message")?;
        Ok(())
    }

    /// Acknowledge every unread message for `agent`. Returns how many.
    ///
    /// # Errors
    /// Returns an error if a query or write fails.
    pub fn ack_all(&self, agent: &str) -> Result<usize> {
        let unread = self.inbox(agent, true, usize::MAX)?;
        for m in &unread {
            self.ack(agent, &m.id)?;
        }
        Ok(unread.len())
    }

    /// How many unread messages `agent` has.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn unread_count(&self, agent: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages m
                 LEFT JOIN message_reads r ON r.message_id = m.id AND r.agent = ?1
                 WHERE (m.to_agent = ?1 OR m.to_agent = '{BROADCAST}') AND m.from_agent != ?1 AND r.read_at IS NULL"
            ),
            params![agent.trim()],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, MailStore) {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("mail.db");
        let s = MailStore::open(&db).expect("open");
        (tmp, s)
    }

    fn reg(s: &MailStore, name: &str) {
        s.register(name, "shell", Some(i64::from(std::process::id())), Path::new(".")).unwrap();
    }

    #[test]
    fn status_and_kind_round_trip() {
        for s in ["idle", "working", "waiting", "offline"] {
            assert_eq!(s.parse::<AgentStatus>().unwrap().as_str(), s);
        }
        for k in ["note", "request", "result", "handoff", "cancel"] {
            assert_eq!(k.parse::<MessageKind>().unwrap().as_str(), k);
        }
        assert_eq!("  WORKING ".parse::<AgentStatus>().unwrap(), AgentStatus::Working);
        assert!("bogus".parse::<AgentStatus>().is_err());
        assert!("bogus".parse::<MessageKind>().is_err());
    }

    #[test]
    fn register_upserts_and_keeps_context() {
        let (_t, s) = store();
        s.register("a", "claude-code", Some(42), Path::new(".")).unwrap();
        s.register("a", "opencode", Some(43), Path::new(".")).unwrap();
        let all = s.list_agents().unwrap();
        assert_eq!(all.len(), 1, "re-register must not duplicate");
        let a = s.get_agent("a").unwrap().unwrap();
        assert_eq!(a.harness, "opencode");
        assert_eq!(a.pid, Some(43));
        // Running inside the smooth repo, so git context should resolve.
        assert!(a.worktree.is_some(), "worktree should be sniffed from cwd");
        assert!(a.branch.is_some(), "branch should be sniffed from cwd");
    }

    #[test]
    fn register_empty_name_rejected() {
        let (_t, s) = store();
        assert!(s.register("   ", "shell", None, Path::new(".")).is_err());
    }

    #[test]
    fn register_resumes_without_clobbering_state() {
        let (_t, s) = store();
        reg(&s, "a");
        reg(&s, "peer");
        s.set_status("a", AgentStatus::Working, Some("shipping th-374f85")).unwrap();
        s.send("peer", "a", "hello", MessageKind::Note, 0, None).unwrap();

        reg(&s, "a"); // resume by name
        let a = s.get_agent("a").unwrap().unwrap();
        assert_eq!(a.status, AgentStatus::Working, "resume must not reset a working agent");
        assert_eq!(a.task.as_deref(), Some("shipping th-374f85"));
        assert_eq!(s.inbox("a", false, 50).unwrap().len(), 1, "history attaches to the resumed handle");
    }

    #[test]
    fn register_revives_an_offline_agent() {
        let (_t, s) = store();
        reg(&s, "a");
        s.set_status("a", AgentStatus::Offline, None).unwrap();
        reg(&s, "a");
        assert_eq!(s.get_agent("a").unwrap().unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn set_status_clears_task_with_empty_string() {
        let (_t, s) = store();
        reg(&s, "a");
        s.set_status("a", AgentStatus::Working, Some("thing")).unwrap();
        s.set_status("a", AgentStatus::Idle, Some("")).unwrap();
        assert!(s.get_agent("a").unwrap().unwrap().task.is_none());
    }

    #[test]
    fn set_status_on_unknown_agent_errors() {
        let (_t, s) = store();
        assert!(s.set_status("ghost", AgentStatus::Idle, None).is_err());
    }

    #[test]
    fn touch_updates_last_seen() {
        let (_t, s) = store();
        reg(&s, "a");
        let before = s.get_agent("a").unwrap().unwrap().last_seen;
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.touch("a").unwrap();
        assert!(s.get_agent("a").unwrap().unwrap().last_seen >= before);
        s.touch("nobody").unwrap(); // no-op, not an error
    }

    #[test]
    fn rename_carries_mail_and_read_state() {
        let (_t, s) = store();
        reg(&s, "cc-smooth-a21c");
        reg(&s, "peer");
        let inbound = s.send("peer", "cc-smooth-a21c", "hi there", MessageKind::Note, 0, None).unwrap();
        s.send("cc-smooth-a21c", "peer", "hi back", MessageKind::Note, 0, None).unwrap();
        s.ack("cc-smooth-a21c", &inbound).unwrap();

        s.rename("cc-smooth-a21c", "fix-auth").unwrap();

        assert!(s.get_agent("cc-smooth-a21c").unwrap().is_none());
        let renamed = s.get_agent("fix-auth").unwrap().expect("renamed present");
        assert_eq!(renamed.harness, "shell");

        let inbox = s.inbox("fix-auth", false, 50).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].body, "hi there");
        assert!(inbox[0].read_at.is_some(), "ack must follow the rename");
        assert_eq!(s.unread_count("fix-auth").unwrap(), 0);
        assert_eq!(s.inbox("peer", false, 50).unwrap()[0].from_agent, "fix-auth");
    }

    #[test]
    fn rename_rejects_empty_missing_and_collisions() {
        let (_t, s) = store();
        reg(&s, "a");
        reg(&s, "b");
        assert!(s.rename("a", "   ").is_err(), "empty target");
        assert!(s.rename("ghost", "whatever").is_err(), "unknown source");
        assert!(s.rename("a", "b").is_err(), "would merge two identities");
        s.rename("a", "a").unwrap(); // no-op
        assert!(s.get_agent("a").unwrap().is_some());
        assert!(s.get_agent("b").unwrap().is_some());
    }

    #[test]
    fn reaper_offlines_dead_pids_only() {
        let (_t, s) = store();
        s.register("live", "shell", Some(i64::from(std::process::id())), Path::new(".")).unwrap();
        // pid 0x7FFFFFFF is well past any real pid on macOS/Linux.
        s.register("dead", "shell", Some(2_147_483_647), Path::new(".")).unwrap();
        s.register("pidless", "shell", None, Path::new(".")).unwrap();

        let agents = s.list_agents().unwrap();
        let by = |n: &str| agents.iter().find(|a| a.name == n).unwrap().status;
        assert_eq!(by("live"), AgentStatus::Idle);
        assert_eq!(by("dead"), AgentStatus::Offline);
        assert_eq!(by("pidless"), AgentStatus::Idle, "no pid = nothing to reap");
    }

    #[test]
    fn direct_mail_only_reaches_its_recipient() {
        let (_t, s) = store();
        s.send("alice", "bob", "hello bob", MessageKind::Note, 0, None).unwrap();
        let bob = s.inbox("bob", false, 50).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].from_agent, "alice");
        assert!(bob[0].read_at.is_none());
        assert!(s.inbox("carol", false, 50).unwrap().is_empty());
        assert!(s.inbox("alice", false, 50).unwrap().is_empty(), "sender must not see its own send");
    }

    #[test]
    fn broadcast_read_state_is_per_recipient() {
        let (_t, s) = store();
        let id = s.send("alice", BROADCAST, "all hands", MessageKind::Note, 0, None).unwrap();
        assert_eq!(s.unread_count("bob").unwrap(), 1);
        assert_eq!(s.unread_count("carol").unwrap(), 1);

        s.ack("bob", &id).unwrap();
        assert_eq!(s.unread_count("bob").unwrap(), 0);
        assert_eq!(s.unread_count("carol").unwrap(), 1, "bob's ack must not consume carol's copy");

        s.ack("carol", &id).unwrap();
        assert_eq!(s.unread_count("carol").unwrap(), 0);
        // Still visible unfiltered, each with its own read_at.
        assert!(s.inbox("bob", false, 50).unwrap()[0].read_at.is_some());
        assert!(s.inbox("carol", false, 50).unwrap()[0].read_at.is_some());
    }

    #[test]
    fn ack_is_idempotent_and_validates_the_id() {
        let (_t, s) = store();
        let id = s.send("a", "bob", "x", MessageKind::Note, 0, None).unwrap();
        s.ack("bob", &id).unwrap();
        s.ack("bob", &id).unwrap();
        assert_eq!(s.unread_count("bob").unwrap(), 0);
        assert!(s.ack("bob", "msg-nope").is_err());
    }

    #[test]
    fn ack_all_clears_the_inbox() {
        let (_t, s) = store();
        s.send("a", "bob", "1", MessageKind::Note, 0, None).unwrap();
        s.send("a", "bob", "2", MessageKind::Request, 0, None).unwrap();
        s.send("a", BROADCAST, "3", MessageKind::Note, 0, None).unwrap();
        assert_eq!(s.unread_count("bob").unwrap(), 3);
        assert_eq!(s.ack_all("bob").unwrap(), 3);
        assert_eq!(s.unread_count("bob").unwrap(), 0);
        assert_eq!(s.ack_all("bob").unwrap(), 0);
        assert_eq!(s.inbox("bob", true, 50).unwrap().len(), 0);
        assert_eq!(s.inbox("bob", false, 50).unwrap().len(), 3);
    }

    #[test]
    fn typed_and_prioritized_mail_round_trips_high_first() {
        let (_t, s) = store();
        s.send("a", "bob", "low", MessageKind::Note, 0, None).unwrap();
        s.send("a", "bob", "urgent", MessageKind::Cancel, 9, None).unwrap();
        let inbox = s.inbox("bob", false, 50).unwrap();
        assert_eq!(inbox[0].body, "urgent", "priority sorts first");
        assert_eq!(inbox[0].kind, MessageKind::Cancel);
        assert_eq!(inbox[0].priority, 9);
        assert_eq!(inbox[1].kind, MessageKind::Note);
    }

    #[test]
    fn threads_group_root_and_replies() {
        let (_t, s) = store();
        let root = s.send("alice", "bob", "question?", MessageKind::Request, 0, None).unwrap();
        s.send("bob", "alice", "answer", MessageKind::Result, 0, Some(&root)).unwrap();
        s.send("alice", "bob", "thanks", MessageKind::Note, 0, Some(&root)).unwrap();
        let thread = s.thread(&root).unwrap();
        assert_eq!(thread.len(), 3);
        assert_eq!(thread[0].id, root);
        assert_eq!(thread[0].thread_root(), root);
        assert_eq!(thread[1].thread_root(), root);
    }

    #[test]
    fn sent_lists_newest_first() {
        let (_t, s) = store();
        s.send("a", "bob", "first", MessageKind::Note, 0, None).unwrap();
        s.send("a", "bob", "second", MessageKind::Note, 0, None).unwrap();
        let sent = s.sent("a", 50).unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].body, "second");
    }

    #[test]
    fn seq_is_monotonic() {
        let (_t, s) = store();
        let mut last = 0;
        for i in 0..10 {
            let id = s.send("a", "bob", &format!("m{i}"), MessageKind::Note, 0, None).unwrap();
            let seq = s.get_message(&id).unwrap().unwrap().seq;
            assert!(seq > last, "seq must increase: {seq} <= {last}");
            last = seq;
        }
    }

    #[test]
    fn empty_fields_rejected_and_bodies_survive_quotes() {
        let (_t, s) = store();
        assert!(s.send("", "bob", "x", MessageKind::Note, 0, None).is_err());
        assert!(s.send("a", "", "x", MessageKind::Note, 0, None).is_err());
        assert!(s.send("a", "bob", "   ", MessageKind::Note, 0, None).is_err());
        s.send("a", "bob", "it's a 'quoted'; DROP TABLE messages; --", MessageKind::Note, 0, None)
            .unwrap();
        assert_eq!(s.inbox("bob", false, 50).unwrap()[0].body, "it's a 'quoted'; DROP TABLE messages; --");
    }

    #[test]
    fn get_message_and_thread_handle_misses() {
        let (_t, s) = store();
        assert!(s.get_message("msg-nope").unwrap().is_none());
        assert!(s.thread("msg-nope").unwrap().is_empty());
        assert!(s.get_agent("nobody").unwrap().is_none());
    }

    #[test]
    fn inbox_respects_limit() {
        let (_t, s) = store();
        for i in 0..5 {
            s.send("a", "bob", &format!("m{i}"), MessageKind::Note, 0, None).unwrap();
        }
        assert_eq!(s.inbox("bob", false, 2).unwrap().len(), 2);
    }

    #[test]
    fn default_path_honors_the_env_override() {
        // Serialized against other env readers by running in-process only here.
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("custom.db");
        std::env::set_var("SMOOTH_MAIL_DB", &db);
        assert_eq!(default_path(), db);
        std::env::remove_var("SMOOTH_MAIL_DB");
        assert!(default_path().ends_with(".smooth/mail.db"));
    }

    /// The whole point of leaving Dolt: many agents writing at once.
    #[test]
    fn concurrent_writers_all_succeed() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("mail.db");
        MailStore::open(&db).unwrap(); // create the schema once up front
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let s = MailStore::open(&db).expect("open");
                    for i in 0..10 {
                        s.send(&format!("sender{t}"), "bob", &format!("t{t}-{i}"), MessageKind::Note, 0, None)
                            .expect("send");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread panicked");
        }
        let s = MailStore::open(&db).unwrap();
        assert_eq!(s.unread_count("bob").unwrap(), 80, "every concurrent send must land");
    }
}
