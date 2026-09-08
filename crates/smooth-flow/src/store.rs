//! SQLite session store — `~/.smooth/flow.db` (`$SMOOTH_FLOW_DB` overrides).
//!
//! Schema per the v0 flow protocol (pinned on epic th-6ac036). Same shape as
//! `smooth_pearls::mail_store`: bundled rusqlite, WAL, a busy timeout so the
//! daemon's supervision tick and a route handler queue on the lock instead of
//! erroring. Timestamps are UTC RFC3339 text written from Rust `Utc::now()`
//! and never compared against SQLite's own `now`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::protocol::{EventKind, FlowEvent};

/// Where the flow database lives: `$SMOOTH_FLOW_DB`, else `~/.smooth/flow.db`.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SMOOTH_FLOW_DB") {
        return PathBuf::from(p);
    }
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join("flow.db")
}

/// What runs in the session's PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    #[default]
    Shell,
    Claude,
    Codex,
    Opencode,
}

impl SessionKind {
    /// The wire/storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
        }
    }

    /// True for kinds that are AI agents (supervised, hook-reporting).
    #[must_use]
    pub const fn is_agent(self) -> bool {
        !matches!(self, Self::Shell)
    }
}

impl FromStr for SessionKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "shell" => Ok(Self::Shell),
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            other => bail!("unknown session kind '{other}' (expected shell|claude|codex|opencode)"),
        }
    }
}

impl std::fmt::Display for SessionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Session lifecycle state. `Done`/`Dead` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Working,
    Idle,
    NeedsYou,
    Limited,
    Done,
    Dead,
}

impl SessionState {
    /// The wire/storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::NeedsYou => "needs_you",
            Self::Limited => "limited",
            Self::Done => "done",
            Self::Dead => "dead",
        }
    }

    /// True once the session can never change state again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Dead)
    }

    /// Whether `self → next` is a legal transition. Terminal states are
    /// sticky; everything live may move anywhere (hooks and scrapes arrive
    /// out of order, so a strict ladder would just drop real signals).
    #[must_use]
    pub const fn can_transition_to(self, _next: Self) -> bool {
        !self.is_terminal()
    }
}

impl FromStr for SessionState {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "starting" => Ok(Self::Starting),
            "working" => Ok(Self::Working),
            "idle" => Ok(Self::Idle),
            "needs_you" => Ok(Self::NeedsYou),
            "limited" => Ok(Self::Limited),
            "done" => Ok(Self::Done),
            "dead" => Ok(Self::Dead),
            other => bail!("unknown session state '{other}'"),
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a session needs the human (or can't proceed). Stored as JSON text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    /// `permission` | `question` | `usage_limit` | `crashed` | `held`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When a `limited` session will be resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_at: Option<DateTime<Utc>>,
    /// Correlates a `permission` attention with the `flow.approve` that answers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Attention {
    /// Build an attention with just a reason.
    #[must_use]
    pub fn new(reason: &str) -> Self {
        Self {
            reason: reason.to_string(),
            detail: None,
            resume_at: None,
            request_id: None,
        }
    }

    /// Attach a human-readable detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// One session row. Serializes as the wire `Session` (no `pid_start`, plus
/// `unread`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub kind: SessionKind,
    pub title: String,
    /// Main repo root (same resolution as pearls: git-common-dir parent).
    pub project: String,
    /// Absolute path the PTY runs in.
    pub worktree: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pearl_id: Option<String>,
    /// Pre-assigned `claude --session-id` uuid / codex id.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// The argv actually launched.
    pub argv: Vec<String>,
    #[serde(default)]
    pub tmux_session: Option<String>,
    /// The tmux socket (`tmux -L …`) the session lives on; `None` on rows
    /// from before th-d33afa ⇒ the daemon's default socket.
    #[serde(default)]
    pub tmux_socket: Option<String>,
    /// How `state` is derived (th-5c5457): `hooks` once the harness has
    /// reported one hook event, else `inferred` (pane scraping).
    #[serde(default = "inferred")]
    pub state_source: String,
    #[serde(default)]
    pub pid: Option<u32>,
    /// Process start time (epoch seconds) — with `pid`, the liveness index.
    #[serde(skip)]
    pub pid_start: Option<i64>,
    pub state: SessionState,
    #[serde(default)]
    pub attention: Option<Attention>,
    #[serde(default)]
    pub fan_out_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub unread: bool,
}

/// A fan-out: N candidate sessions racing one prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanOut {
    pub id: String,
    pub prompt: String,
    pub base_commit: String,
    pub pearl_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub winner_session_id: Option<String>,
}

/// Fields set at creation; everything else defaults.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub kind: Option<SessionKind>,
    pub title: String,
    pub project: String,
    pub worktree: String,
    pub branch: Option<String>,
    pub pearl_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub argv: Vec<String>,
    pub tmux_session: Option<String>,
    pub tmux_socket: Option<String>,
    pub fan_out_id: Option<String>,
}

/// Mint a session id: `fs-` + 8 hex.
#[must_use]
pub fn new_session_id() -> String {
    format!("fs-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// Events kept per session (th-d33afa): the phone's Chat tab replays these
/// on `flow.attach`.
pub const EVENT_BUFFER: usize = 200;

/// `<session>-<seq>` — the mock's spelling, so phones need no special case.
fn event_id(session_id: &str, seq: i64) -> String {
    format!("{session_id}-{seq}")
}

/// Mint a fan-out id: `fo-` + 8 hex.
#[must_use]
pub fn new_fan_out_id() -> String {
    format!("fo-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn inferred() -> String {
    "inferred".to_string()
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc))
}

/// The store. One connection; callers serialize through a `Mutex`.
pub struct FlowStore {
    conn: Connection,
}

impl FlowStore {
    /// Open (creating if needed) the default database.
    ///
    /// # Errors
    /// When the file cannot be opened or the schema cannot be applied.
    pub fn open_default() -> Result<Self> {
        Self::open(&default_path())
    }

    /// Open (creating if needed) the database at `path`.
    ///
    /// # Errors
    /// When the file cannot be opened or the schema cannot be applied.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open flow db {}", path.display()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
        conn.busy_timeout(std::time::Duration::from_secs(10)).context("set busy_timeout")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id               TEXT PRIMARY KEY,
                 kind             TEXT NOT NULL,
                 title            TEXT NOT NULL DEFAULT '',
                 project          TEXT NOT NULL,
                 worktree         TEXT NOT NULL,
                 branch           TEXT,
                 pearl_id         TEXT,
                 agent_session_id TEXT,
                 argv             TEXT NOT NULL DEFAULT '[]',
                 tmux_session     TEXT,
                 pid              INTEGER,
                 pid_start        INTEGER,
                 state            TEXT NOT NULL,
                 attention        TEXT,
                 fan_out_id       TEXT,
                 created_at       TEXT NOT NULL,
                 updated_at       TEXT NOT NULL,
                 ended_at         TEXT,
                 exit_code        INTEGER,
                 unread           INTEGER NOT NULL DEFAULT 0,
                 tmux_socket      TEXT,
                 state_source     TEXT NOT NULL DEFAULT 'inferred'
             );
             CREATE INDEX IF NOT EXISTS sessions_agent_idx ON sessions(agent_session_id);
             CREATE INDEX IF NOT EXISTS sessions_fanout_idx ON sessions(fan_out_id);
             CREATE TABLE IF NOT EXISTS events (
                 session_id TEXT NOT NULL,
                 seq        INTEGER NOT NULL,
                 at         TEXT NOT NULL,
                 kind       TEXT NOT NULL,
                 text       TEXT NOT NULL,
                 PRIMARY KEY (session_id, seq)
             );
             CREATE TABLE IF NOT EXISTS fan_outs (
                 id                TEXT PRIMARY KEY,
                 prompt            TEXT NOT NULL,
                 base_commit       TEXT NOT NULL,
                 pearl_id          TEXT NOT NULL,
                 created_at        TEXT NOT NULL,
                 winner_session_id TEXT
             );",
        )
        .context("apply flow schema")?;
        // th-d33afa added `tmux_socket` to an existing table; SQLite has no
        // ADD COLUMN IF NOT EXISTS, so probe first.
        let has_socket = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'tmux_socket'")?
            .exists([])
            .context("probe tmux_socket column")?;
        if !has_socket {
            conn.execute("ALTER TABLE sessions ADD COLUMN tmux_socket TEXT", [])
                .context("add tmux_socket")?;
        }
        let has_source = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'state_source'")?
            .exists([])
            .context("probe state_source column")?;
        if !has_source {
            conn.execute("ALTER TABLE sessions ADD COLUMN state_source TEXT NOT NULL DEFAULT 'inferred'", [])
                .context("add state_source")?;
        }
        Ok(Self { conn })
    }

    /// In-memory store (tests).
    ///
    /// # Errors
    /// When the schema cannot be applied.
    pub fn open_in_memory() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("smooth-flow-{}.db", uuid::Uuid::new_v4().simple()));
        Self::open(&dir)
    }

    fn row_to_session(row: &Row<'_>) -> rusqlite::Result<Session> {
        let argv: String = row.get("argv")?;
        let attention: Option<String> = row.get("attention")?;
        let created: String = row.get("created_at")?;
        let updated: String = row.get("updated_at")?;
        let ended: Option<String> = row.get("ended_at")?;
        let kind: String = row.get("kind")?;
        let state: String = row.get("state")?;
        Ok(Session {
            id: row.get("id")?,
            kind: kind.parse().unwrap_or(SessionKind::Shell),
            title: row.get("title")?,
            project: row.get("project")?,
            worktree: row.get("worktree")?,
            branch: row.get("branch")?,
            pearl_id: row.get("pearl_id")?,
            agent_session_id: row.get("agent_session_id")?,
            argv: serde_json::from_str(&argv).unwrap_or_default(),
            tmux_session: row.get("tmux_session")?,
            tmux_socket: row.get("tmux_socket")?,
            state_source: row.get("state_source")?,
            pid: row.get::<_, Option<i64>>("pid")?.and_then(|p| u32::try_from(p).ok()),
            pid_start: row.get("pid_start")?,
            state: state.parse().unwrap_or(SessionState::Dead),
            attention: attention.and_then(|a| serde_json::from_str(&a).ok()),
            fan_out_id: row.get("fan_out_id")?,
            created_at: parse_ts(&created),
            updated_at: parse_ts(&updated),
            ended_at: ended.as_deref().map(parse_ts),
            exit_code: row.get("exit_code")?,
            unread: row.get::<_, i64>("unread")? != 0,
        })
    }

    /// Insert a new session in `starting` state and return it.
    ///
    /// # Errors
    /// On a database failure.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(&self, new: NewSession) -> Result<Session> {
        let now = Utc::now();
        let id = new_session_id();
        let kind = new.kind.unwrap_or(SessionKind::Shell);
        self.conn
            .execute(
                "INSERT INTO sessions (id, kind, title, project, worktree, branch, pearl_id, agent_session_id, argv, tmux_session,
                                       state, fan_out_id, created_at, updated_at, tmux_socket)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'starting', ?11, ?12, ?12, ?13)",
                params![
                    id,
                    kind.as_str(),
                    new.title,
                    new.project,
                    new.worktree,
                    new.branch,
                    new.pearl_id,
                    new.agent_session_id,
                    serde_json::to_string(&new.argv)?,
                    new.tmux_session,
                    new.fan_out_id,
                    now.to_rfc3339(),
                    new.tmux_socket,
                ],
            )
            .context("insert session")?;
        self.get(&id)?.context("session vanished after insert")
    }

    /// One session by id.
    ///
    /// # Errors
    /// On a database failure.
    pub fn get(&self, id: &str) -> Result<Option<Session>> {
        self.conn
            .query_row("SELECT * FROM sessions WHERE id = ?1", params![id], Self::row_to_session)
            .optional()
            .context("get session")
    }

    /// The session whose pre-assigned harness id is `agent_session_id`.
    ///
    /// # Errors
    /// On a database failure.
    pub fn get_by_agent_session(&self, agent_session_id: &str) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT * FROM sessions WHERE agent_session_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![agent_session_id],
                Self::row_to_session,
            )
            .optional()
            .context("get session by agent id")
    }

    /// Every session, newest first.
    ///
    /// # Errors
    /// On a database failure.
    pub fn list(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], Self::row_to_session)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("list sessions")
    }

    /// Sessions that are not terminal.
    ///
    /// # Errors
    /// On a database failure.
    pub fn list_live(&self) -> Result<Vec<Session>> {
        Ok(self.list()?.into_iter().filter(|s| !s.state.is_terminal()).collect())
    }

    /// Candidates of one fan-out.
    ///
    /// # Errors
    /// On a database failure.
    pub fn list_by_fan_out(&self, fan_out_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions WHERE fan_out_id = ?1 ORDER BY created_at ASC")?;
        let rows = stmt.query_map(params![fan_out_id], Self::row_to_session)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("list fan-out sessions")
    }

    /// Move a session to `state`, replacing its attention. Terminal states
    /// are sticky: a transition out of `done`/`dead` is refused (returns the
    /// unchanged row). Returns `None` for an unknown id.
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_state(&self, id: &str, state: SessionState, attention: Option<&Attention>) -> Result<Option<Session>> {
        let Some(cur) = self.get(id)? else { return Ok(None) };
        if !cur.state.can_transition_to(state) {
            return Ok(Some(cur));
        }
        let now = Utc::now();
        let ended = state.is_terminal().then(|| now.to_rfc3339());
        self.conn
            .execute(
                "UPDATE sessions SET state = ?2, attention = ?3, updated_at = ?4, ended_at = COALESCE(ended_at, ?5) WHERE id = ?1",
                params![id, state.as_str(), attention.map(serde_json::to_string).transpose()?, now.to_rfc3339(), ended],
            )
            .context("set state")?;
        self.get(id)
    }

    /// Record the launched process identity (tmux session, pid, start time)
    /// and argv — after a launch or a relaunch.
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_process(&self, id: &str, tmux_session: Option<&str>, pid: Option<u32>, pid_start: Option<i64>, argv: &[String]) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET tmux_session = ?2, pid = ?3, pid_start = ?4, argv = ?5, updated_at = ?6 WHERE id = ?1",
                params![
                    id,
                    tmux_session,
                    pid.map(i64::from),
                    pid_start,
                    serde_json::to_string(argv)?,
                    Utc::now().to_rfc3339()
                ],
            )
            .context("set process")?;
        Ok(())
    }

    /// Record the exit code the PTY reported.
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_exit_code(&self, id: &str, code: Option<i32>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET exit_code = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, code, Utc::now().to_rfc3339()],
            )
            .context("set exit code")?;
        Ok(())
    }

    /// Flip the unread flag.
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_unread(&self, id: &str, unread: bool) -> Result<()> {
        self.conn
            .execute("UPDATE sessions SET unread = ?2 WHERE id = ?1", params![id, i64::from(unread)])
            .context("set unread")?;
        Ok(())
    }

    /// Update title / branch / pearl (any `Some` is applied).
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_meta(&self, id: &str, title: Option<&str>, branch: Option<&str>, pearl_id: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET title = COALESCE(?2, title), branch = COALESCE(?3, branch), pearl_id = COALESCE(?4, pearl_id), updated_at = ?5
                 WHERE id = ?1",
                params![id, title, branch, pearl_id, Utc::now().to_rfc3339()],
            )
            .context("set meta")?;
        Ok(())
    }

    /// Delete a session row.
    ///
    /// # Errors
    /// On a database failure.
    pub fn remove(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM sessions WHERE id = ?1", params![id]).context("remove session")? > 0)
    }

    /// Mark how the session's state is derived (th-5c5457).
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_state_source(&self, id: &str, source: &str) -> Result<()> {
        self.conn
            .execute("UPDATE sessions SET state_source = ?2 WHERE id = ?1", params![id, source])
            .context("set state_source")?;
        Ok(())
    }

    /// Bind a harness session id learned from its first hook to a row that
    /// launched without one (opencode / codex can't pre-assign ids).
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_agent_session(&self, id: &str, agent_session_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET agent_session_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, agent_session_id, Utc::now().to_rfc3339()],
            )
            .context("set agent_session_id")?;
        Ok(())
    }

    /// The newest live agent session running in `worktree` that has no
    /// harness session id yet — the row a first hook from that cwd binds to.
    ///
    /// # Errors
    /// On a database failure.
    pub fn find_bindable(&self, worktree: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sessions WHERE worktree = ?1 AND agent_session_id IS NULL AND kind != 'shell'
             AND state NOT IN ('done', 'dead') ORDER BY created_at DESC LIMIT 1",
        )?;
        stmt.query_row(params![worktree], Self::row_to_session)
            .optional()
            .context("find bindable session")
    }

    /// Append one event line to `session_id`'s stream (`flow.event`,
    /// th-d33afa) and prune the stream to the last [`EVENT_BUFFER`].
    ///
    /// # Errors
    /// On a database failure.
    pub fn add_event(&self, session_id: &str, kind: EventKind, text: &str) -> Result<FlowEvent> {
        let seq: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1", params![session_id], |r| {
                r.get(0)
            })
            .context("next event seq")?;
        let at = Utc::now();
        // Guarded on the session row (no FK in the schema): a stray id must
        // not grow the table.
        let n = self
            .conn
            .execute(
                "INSERT INTO events (session_id, seq, at, kind, text)
                 SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
                params![session_id, seq, at.to_rfc3339(), kind.as_str(), text],
            )
            .context("insert event")?;
        if n == 0 {
            bail!("no such session: {session_id}");
        }
        self.conn
            .execute(
                "DELETE FROM events WHERE session_id = ?1 AND seq <= ?2 - ?3",
                params![session_id, seq, i64::try_from(EVENT_BUFFER).unwrap_or(i64::MAX)],
            )
            .context("prune events")?;
        Ok(FlowEvent {
            event_id: event_id(session_id, seq),
            at,
            kind,
            text: text.to_string(),
        })
    }

    /// The buffered event stream of `session_id`, oldest first.
    ///
    /// # Errors
    /// On a database failure.
    pub fn events(&self, session_id: &str) -> Result<Vec<FlowEvent>> {
        let mut stmt = self.conn.prepare("SELECT seq, at, kind, text FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![session_id], |r| {
            let seq: i64 = r.get(0)?;
            let at: String = r.get(1)?;
            let kind: String = r.get(2)?;
            Ok(FlowEvent {
                event_id: event_id(session_id, seq),
                at: parse_ts(&at),
                kind: kind.parse().unwrap_or(EventKind::System),
                text: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("list events")
    }

    /// Insert a fan-out.
    ///
    /// # Errors
    /// On a database failure.
    pub fn create_fan_out(&self, prompt: &str, base_commit: &str, pearl_id: &str) -> Result<FanOut> {
        let fo = FanOut {
            id: new_fan_out_id(),
            prompt: prompt.to_string(),
            base_commit: base_commit.to_string(),
            pearl_id: pearl_id.to_string(),
            created_at: Utc::now(),
            winner_session_id: None,
        };
        self.conn
            .execute(
                "INSERT INTO fan_outs (id, prompt, base_commit, pearl_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![fo.id, fo.prompt, fo.base_commit, fo.pearl_id, fo.created_at.to_rfc3339()],
            )
            .context("insert fan-out")?;
        Ok(fo)
    }

    /// One fan-out by id.
    ///
    /// # Errors
    /// On a database failure.
    pub fn get_fan_out(&self, id: &str) -> Result<Option<FanOut>> {
        self.conn
            .query_row("SELECT * FROM fan_outs WHERE id = ?1", params![id], |row| {
                let created: String = row.get("created_at")?;
                Ok(FanOut {
                    id: row.get("id")?,
                    prompt: row.get("prompt")?,
                    base_commit: row.get("base_commit")?,
                    pearl_id: row.get("pearl_id")?,
                    created_at: parse_ts(&created),
                    winner_session_id: row.get("winner_session_id")?,
                })
            })
            .optional()
            .context("get fan-out")
    }

    /// Record the winner.
    ///
    /// # Errors
    /// On a database failure.
    pub fn set_fan_out_winner(&self, id: &str, winner: &str) -> Result<()> {
        self.conn
            .execute("UPDATE fan_outs SET winner_session_id = ?2 WHERE id = ?1", params![id, winner])
            .context("set fan-out winner")?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn store() -> FlowStore {
        FlowStore::open_in_memory().unwrap()
    }

    fn new_shell() -> NewSession {
        NewSession {
            kind: Some(SessionKind::Shell),
            title: "sh".into(),
            project: "/p".into(),
            worktree: "/p".into(),
            argv: vec!["zsh".into(), "-l".into()],
            ..Default::default()
        }
    }

    /// th-d33afa: the event stream is capped at EVENT_BUFFER per session,
    /// ids stay monotonic across the prune, and a stray session id is refused.
    #[test]
    fn events_buffer_last_200_per_session_and_refuse_ghosts() {
        let st = FlowStore::open_in_memory().unwrap();
        let s = st
            .create(NewSession {
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap();
        for i in 1..=(EVENT_BUFFER + 5) {
            let ev = st.add_event(&s.id, EventKind::Tool, &format!("line {i}")).unwrap();
            assert_eq!(ev.event_id, format!("{}-{i}", s.id));
        }
        let evs = st.events(&s.id).unwrap();
        assert_eq!(evs.len(), EVENT_BUFFER);
        assert_eq!(evs.first().unwrap().text, "line 6");
        assert_eq!(evs.last().unwrap().event_id, format!("{}-{}", s.id, EVENT_BUFFER + 5));
        assert!(st.add_event("fs-ghost", EventKind::System, "x").is_err());
        assert!(st.events("fs-ghost").unwrap().is_empty());
        // Streams are per session.
        let other = st
            .create(NewSession {
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(st.events(&other.id).unwrap().is_empty());
    }

    /// th-d33afa: a pre-existing flow.db without `tmux_socket` is migrated on
    /// open, and rows from before carry `None`.
    #[test]
    fn opening_an_old_db_adds_the_tmux_socket_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', project TEXT NOT NULL,
                 worktree TEXT NOT NULL, branch TEXT, pearl_id TEXT, agent_session_id TEXT, argv TEXT NOT NULL DEFAULT '[]',
                 tmux_session TEXT, pid INTEGER, pid_start INTEGER, state TEXT NOT NULL, attention TEXT, fan_out_id TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, ended_at TEXT, exit_code INTEGER, unread INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO sessions (id, kind, project, worktree, state, created_at, updated_at)
                 VALUES ('fs-old', 'shell', '/p', '/p', 'idle', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let st = FlowStore::open(&path).unwrap();
        assert_eq!(st.get("fs-old").unwrap().unwrap().tmux_socket, None);
        assert_eq!(st.get("fs-old").unwrap().unwrap().state_source, "inferred", "th-5c5457 column migrated too");
        st.set_state_source("fs-old", "hooks").unwrap();
        assert_eq!(st.get("fs-old").unwrap().unwrap().state_source, "hooks");
        let st2 = FlowStore::open(&path).unwrap(); // idempotent
        let s = st2
            .create(NewSession {
                project: "/p".into(),
                worktree: "/p".into(),
                tmux_socket: Some("smoothflow".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(st2.get(&s.id).unwrap().unwrap().tmux_socket.as_deref(), Some("smoothflow"));
    }

    #[test]
    fn ids_have_the_documented_shape() {
        let id = new_session_id();
        assert!(id.starts_with("fs-") && id.len() == 11, "{id}");
        assert!(id[3..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(new_fan_out_id().starts_with("fo-"));
        assert_ne!(new_session_id(), new_session_id());
    }

    #[test]
    fn kind_and_state_round_trip_through_strings() {
        for k in [SessionKind::Shell, SessionKind::Claude, SessionKind::Codex, SessionKind::Opencode] {
            assert_eq!(k.as_str().parse::<SessionKind>().unwrap(), k);
        }
        for s in [
            SessionState::Starting,
            SessionState::Working,
            SessionState::Idle,
            SessionState::NeedsYou,
            SessionState::Limited,
            SessionState::Done,
            SessionState::Dead,
        ] {
            assert_eq!(s.as_str().parse::<SessionState>().unwrap(), s);
        }
        assert!("bogus".parse::<SessionKind>().is_err());
        assert!("bogus".parse::<SessionState>().is_err());
        assert!(SessionKind::Claude.is_agent() && !SessionKind::Shell.is_agent());
    }

    #[test]
    fn create_get_list_remove() {
        let st = store();
        let s = st.create(new_shell()).unwrap();
        assert_eq!(s.state, SessionState::Starting);
        assert_eq!(s.argv, vec!["zsh", "-l"]);
        assert!(!s.unread);
        assert_eq!(st.get(&s.id).unwrap().unwrap(), s);
        assert_eq!(st.list().unwrap().len(), 1);
        assert_eq!(st.list_live().unwrap().len(), 1);
        assert!(st.remove(&s.id).unwrap());
        assert!(!st.remove(&s.id).unwrap());
        assert!(st.get(&s.id).unwrap().is_none());
    }

    #[test]
    fn state_machine_live_moves_anywhere_terminal_is_sticky() {
        let st = store();
        let s = st.create(new_shell()).unwrap();
        let att = Attention::new("permission").with_detail("Bash: rm -rf");
        let s2 = st.set_state(&s.id, SessionState::NeedsYou, Some(&att)).unwrap().unwrap();
        assert_eq!(s2.state, SessionState::NeedsYou);
        assert_eq!(s2.attention.as_ref().unwrap().detail.as_deref(), Some("Bash: rm -rf"));
        assert!(s2.ended_at.is_none());

        let s3 = st.set_state(&s.id, SessionState::Working, None).unwrap().unwrap();
        assert_eq!(s3.state, SessionState::Working);
        assert!(s3.attention.is_none(), "attention is replaced on every transition");

        let s4 = st.set_state(&s.id, SessionState::Done, None).unwrap().unwrap();
        assert_eq!(s4.state, SessionState::Done);
        assert!(s4.ended_at.is_some());
        assert!(st.list_live().unwrap().is_empty());

        let s5 = st.set_state(&s.id, SessionState::Working, None).unwrap().unwrap();
        assert_eq!(s5.state, SessionState::Done, "done is sticky");
        assert!(st.set_state("fs-nope", SessionState::Idle, None).unwrap().is_none());
    }

    #[test]
    fn transition_table() {
        use SessionState as S;
        assert!(S::Starting.can_transition_to(S::Working));
        assert!(S::Working.can_transition_to(S::Idle));
        assert!(S::Idle.can_transition_to(S::NeedsYou));
        assert!(S::NeedsYou.can_transition_to(S::Working));
        assert!(S::Limited.can_transition_to(S::Working));
        assert!(S::Working.can_transition_to(S::Dead));
        assert!(!S::Done.can_transition_to(S::Working));
        assert!(!S::Dead.can_transition_to(S::Idle));
        assert!(!S::Dead.can_transition_to(S::Done));
    }

    #[test]
    fn process_exit_unread_meta_updates() {
        let st = store();
        let s = st.create(new_shell()).unwrap();
        st.set_process(&s.id, Some("flow-x"), Some(4242), Some(1_700_000_000), &["claude".into(), "--resume".into()])
            .unwrap();
        st.set_exit_code(&s.id, Some(3)).unwrap();
        st.set_unread(&s.id, true).unwrap();
        st.set_meta(&s.id, Some("renamed"), Some("feat/x"), Some("th-abc123")).unwrap();
        let s = st.get(&s.id).unwrap().unwrap();
        assert_eq!(s.tmux_session.as_deref(), Some("flow-x"));
        assert_eq!(s.pid, Some(4242));
        assert_eq!(s.pid_start, Some(1_700_000_000));
        assert_eq!(s.argv, vec!["claude", "--resume"]);
        assert_eq!(s.exit_code, Some(3));
        assert!(s.unread);
        assert_eq!(s.title, "renamed");
        assert_eq!(s.branch.as_deref(), Some("feat/x"));
        assert_eq!(s.pearl_id.as_deref(), Some("th-abc123"));
        // COALESCE keeps existing values on None.
        st.set_meta(&s.id, None, None, None).unwrap();
        assert_eq!(st.get(&s.id).unwrap().unwrap().title, "renamed");
    }

    #[test]
    fn lookup_by_agent_session_id() {
        let st = store();
        let mut n = new_shell();
        n.kind = Some(SessionKind::Claude);
        n.agent_session_id = Some("uuid-1".into());
        let s = st.create(n).unwrap();
        assert_eq!(st.get_by_agent_session("uuid-1").unwrap().unwrap().id, s.id);
        assert!(st.get_by_agent_session("uuid-2").unwrap().is_none());
    }

    #[test]
    fn fan_out_crud() {
        let st = store();
        let fo = st.create_fan_out("do the thing", "abc123", "th-000001").unwrap();
        assert!(fo.winner_session_id.is_none());
        let mut n = new_shell();
        n.fan_out_id = Some(fo.id.clone());
        let a = st.create(n.clone()).unwrap();
        let b = st.create(n).unwrap();
        let cands = st.list_by_fan_out(&fo.id).unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].id, a.id);
        st.set_fan_out_winner(&fo.id, &b.id).unwrap();
        assert_eq!(st.get_fan_out(&fo.id).unwrap().unwrap().winner_session_id.as_deref(), Some(b.id.as_str()));
        assert!(st.get_fan_out("fo-nope").unwrap().is_none());
    }

    #[test]
    fn session_wire_shape_hides_pid_start_and_shows_unread() {
        let st = store();
        let s = st.create(new_shell()).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("pid_start").is_none());
        assert_eq!(v["unread"], false);
        assert_eq!(v["state"], "starting");
        assert_eq!(v["kind"], "shell");
        let back: Session = serde_json::from_value(v).unwrap();
        assert_eq!(back.id, s.id);
    }

    #[test]
    fn attention_serializes_compactly() {
        let a = Attention::new("held");
        assert_eq!(serde_json::to_string(&a).unwrap(), r#"{"reason":"held"}"#);
        let b: Attention = serde_json::from_str(r#"{"reason":"usage_limit","resume_at":"2026-09-07T22:00:00Z"}"#).unwrap();
        assert!(b.resume_at.is_some());
    }

    #[test]
    fn default_path_honors_the_env_override() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("flow.db");
        std::env::set_var("SMOOTH_FLOW_DB", &db);
        let got = default_path();
        std::env::remove_var("SMOOTH_FLOW_DB");
        assert_eq!(got, db);
    }

    #[test]
    fn reopen_keeps_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("flow.db");
        let id = FlowStore::open(&db).unwrap().create(new_shell()).unwrap().id;
        assert!(FlowStore::open(&db).unwrap().get(&id).unwrap().is_some());
    }
}
