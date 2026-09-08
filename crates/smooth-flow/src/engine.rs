//! The engine: sessions, PTY streaming, hooks, approvals, supervision.
//!
//! One `Engine` per daemon (cheaply cloneable — `Arc` inside). Every state
//! change is written to the store and then broadcast as a [`ServerFrame`]
//! to every subscriber (the daemon's WS handler fans it out per client).
//! Methods are synchronous and may shell out (tmux, git, `th`); the daemon
//! calls the slow ones from `spawn_blocking`. The supervision tick
//! ([`Engine::supervise_tick`]) is driven by a tokio interval in the host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use smooth_tmux::detect::{detect_state, PaneState};
use tokio::sync::{broadcast, oneshot};

use crate::protocol::{approval_keystroke, map_hook_event, permission_reply, CandidateSpec, DaemonInfo, Decision, HookEvent, HookOutcome, ServerFrame};
use crate::pty::{OnOutput, PtyAttach};
use crate::store::{Attention, FanOut, FlowStore, NewSession, Session, SessionKind, SessionState};
use crate::{limit, proc, tmux};

/// Supervision rule 2: relaunch attempts before `dead`.
pub const MAX_RESUME_ATTEMPTS: u32 = 3;
/// Base of the exponential backoff between relaunches.
pub const RESUME_BACKOFF_BASE: Duration = Duration::from_secs(5);
/// Rule 4: how long a resume claim on an agent session id is honoured.
pub const RESUME_CLAIM_TTL: Duration = Duration::from_secs(60);
/// After a usage-limit resume, ignore the (stale) limit banner this long.
const LIMIT_REARM_GRACE: Duration = Duration::from_secs(90);
/// Broadcast capacity — output chunks are ≤16 KiB, so this is ~64 MiB
/// worst case before a slow subscriber sees `Lagged`.
const BROADCAST_CAPACITY: usize = 4096;
/// Kill grace before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// How the engine is configured by its host.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// SQLite path (`store::default_path()` by default).
    pub db_path: PathBuf,
    /// The main checkout new sessions default to.
    pub default_project: PathBuf,
    /// Reported in `flow.hello`.
    pub version: String,
    pub machine_label: String,
}

impl EngineConfig {
    /// Defaults: `~/.smooth/flow.db`, `default_project`, this crate's version,
    /// the short hostname.
    #[must_use]
    pub fn new(default_project: PathBuf) -> Self {
        Self {
            db_path: crate::store::default_path(),
            default_project,
            version: env!("CARGO_PKG_VERSION").to_string(),
            machine_label: short_hostname(),
        }
    }
}

/// A `flow.new` request (the wire frame minus the tag).
#[derive(Debug, Clone, Default)]
pub struct NewRequest {
    pub kind: SessionKind,
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub pearl_id: Option<String>,
    pub prompt: Option<String>,
    pub argv: Option<Vec<String>>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub fan_out_id: Option<String>,
}

/// What `POST /api/flow/hooks` should do after the engine processed a hook.
pub enum HookReply {
    /// Reply with this body now.
    Immediate(Value),
    /// A permission request: wait (≤ the host's cap) for a decision; the
    /// payload builds the harness reply. Timing out ⇒ reply `{}`.
    Pending {
        request_id: String,
        rx: oneshot::Receiver<Decision>,
        payload: Value,
    },
}

struct PendingApproval {
    session_id: String,
    tx: Option<oneshot::Sender<Decision>>,
}

#[derive(Default)]
struct Runtime {
    /// Sessions that have reported at least one hook (scraping then only
    /// covers what hooks can't: usage limits and approvals hooks missed).
    hook_seen: std::collections::HashSet<String>,
    resume_attempts: HashMap<String, u32>,
    /// Session id → when its pending relaunch may fire.
    relaunch_at: HashMap<String, Instant>,
    /// Agent session id → (pid, claimed at): rule 4's duplicate-resume guard.
    claims: HashMap<String, (u32, Instant)>,
    /// Session id → when it was last resumed from a usage limit.
    limit_resumed_at: HashMap<String, Instant>,
}

struct Inner {
    store: Mutex<FlowStore>,
    tx: broadcast::Sender<ServerFrame>,
    ptys: Mutex<HashMap<String, Arc<PtyAttach>>>,
    pending: Mutex<HashMap<String, PendingApproval>>,
    rt: Mutex<Runtime>,
    info: DaemonInfo,
    default_project: PathBuf,
}

/// The SmoothFlow engine handle.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

fn short_hostname() -> String {
    let raw = Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string());
    raw.split('.').next().unwrap_or(&raw).to_string()
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The `th` binary to shell out to (`$SMOOTH_TH_BIN` overrides).
fn th_bin() -> String {
    std::env::var("SMOOTH_TH_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "th".to_string())
}

fn th(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(th_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("th {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("th {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The main checkout for `dir` (git-common-dir's parent — the pearls rule),
/// or `dir` itself outside a repo.
#[must_use]
pub fn project_root(dir: &Path) -> PathBuf {
    git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .ok()
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// A filesystem-safe slug for worktree/branch names.
#[must_use]
pub fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= max {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// Build the argv for a fresh session of `kind`.
#[must_use]
pub fn default_argv(kind: SessionKind, agent_session_id: Option<&str>, model: Option<&str>, prompt: Option<&str>) -> Vec<String> {
    match kind {
        SessionKind::Shell => {
            let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
            vec![shell, "-l".into()]
        }
        SessionKind::Claude => {
            let mut v = vec!["claude".to_string()];
            if let Some(id) = agent_session_id {
                v.push("--session-id".into());
                v.push(id.into());
            }
            if let Some(m) = model {
                v.push("--model".into());
                v.push(m.into());
            }
            if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
                v.push(p.into());
            }
            v
        }
        SessionKind::Codex | SessionKind::Opencode => {
            let mut v = vec![kind.as_str().to_string()];
            if let Some(m) = model {
                v.push("--model".into());
                v.push(m.into());
            }
            if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
                v.push(p.into());
            }
            v
        }
    }
}

/// The argv that resumes a dead agent session (`claude --resume <id>`); other
/// kinds relaunch their original argv.
#[must_use]
pub fn resume_argv(session: &Session) -> Vec<String> {
    match (session.kind, session.agent_session_id.as_deref()) {
        (SessionKind::Claude, Some(id)) => vec!["claude".into(), "--resume".into(), id.into()],
        _ => session.argv.clone(),
    }
}

/// Rule 2's backoff: `base · 2^attempt`.
#[must_use]
pub fn resume_backoff(attempt: u32) -> Duration {
    RESUME_BACKOFF_BASE.saturating_mul(2u32.saturating_pow(attempt.min(10)))
}

/// Default title for a new session.
#[must_use]
pub fn default_title(kind: SessionKind, prompt: Option<&str>, pearl_id: Option<&str>, worktree: &Path) -> String {
    if let Some(p) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        let short: String = p.chars().take(60).collect();
        return short;
    }
    if let Some(p) = pearl_id {
        return p.to_string();
    }
    let dir = worktree.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{kind} · {dir}")
}

/// Rule 4, pure: is `agent_session_id` currently claimed by a live pid?
/// `claims` is (pid, claimed_at) per id; `alive` decides liveness.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn claim_holder(claims: &HashMap<String, (u32, Instant)>, agent_session_id: &str, now: Instant, alive: impl Fn(u32) -> bool) -> Option<u32> {
    let (pid, at) = claims.get(agent_session_id)?;
    if now.duration_since(*at) > RESUME_CLAIM_TTL {
        return None;
    }
    alive(*pid).then_some(*pid)
}

impl Engine {
    /// Open the store and build the engine. Reconciles nothing eagerly —
    /// the first supervision tick does.
    ///
    /// # Errors
    /// When the store cannot be opened.
    pub fn open(cfg: EngineConfig) -> Result<Self> {
        let store = FlowStore::open(&cfg.db_path)?;
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            inner: Arc::new(Inner {
                store: Mutex::new(store),
                tx,
                ptys: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                rt: Mutex::new(Runtime::default()),
                info: DaemonInfo {
                    version: cfg.version,
                    machine_label: cfg.machine_label,
                },
                default_project: cfg.default_project,
            }),
        })
    }

    /// Subscribe to every broadcast frame.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerFrame> {
        self.inner.tx.subscribe()
    }

    fn with_store<T>(&self, f: impl FnOnce(&FlowStore) -> Result<T>) -> Result<T> {
        let st = self.inner.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&st)
    }

    fn rt(&self) -> std::sync::MutexGuard<'_, Runtime> {
        self.inner.rt.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn emit(&self, frame: ServerFrame) {
        let _ = self.inner.tx.send(frame);
    }

    fn emit_session(&self, s: &Session) {
        self.emit(ServerFrame::Session { session: s.clone() });
    }

    /// The `flow.hello` frame for a new client.
    ///
    /// # Errors
    /// On a store failure.
    pub fn hello(&self) -> Result<ServerFrame> {
        Ok(ServerFrame::Hello {
            daemon: self.inner.info.clone(),
            sessions: self.list()?,
        })
    }

    /// All sessions, newest first.
    ///
    /// # Errors
    /// On a store failure.
    pub fn list(&self) -> Result<Vec<Session>> {
        self.with_store(FlowStore::list)
    }

    /// One session.
    ///
    /// # Errors
    /// On a store failure; `Ok(None)` for an unknown id.
    pub fn get(&self, id: &str) -> Result<Option<Session>> {
        self.with_store(|st| st.get(id))
    }

    fn require(&self, id: &str) -> Result<Session> {
        self.get(id)?.ok_or_else(|| anyhow!("no such session: {id}"))
    }

    /// Transition + broadcast (`flow.session`, and `flow.attention` when the
    /// attention changed).
    #[allow(clippy::needless_pass_by_value)]
    fn set_state(&self, id: &str, state: SessionState, attention: Option<Attention>) -> Result<Option<Session>> {
        let before = self.get(id)?;
        let after = self.with_store(|st| st.set_state(id, state, attention.as_ref()))?;
        if let Some(s) = &after {
            self.emit_session(s);
            if before.as_ref().map(|b| &b.attention) != Some(&s.attention) {
                self.emit(ServerFrame::Attention {
                    id: id.to_string(),
                    attention: s.attention.clone(),
                });
            }
        }
        Ok(after)
    }

    /// Create a worktree `../<repo>-<pearl>-<slug>` on branch `<pearl>-<slug>`
    /// from `project`'s current HEAD (idempotent when it already exists).
    ///
    /// # Errors
    /// When git refuses.
    pub fn create_worktree(project: &Path, pearl_id: &str, slug: &str, base: &str) -> Result<PathBuf> {
        let repo = project.file_name().map_or_else(|| "repo".into(), |f| f.to_string_lossy().into_owned());
        let branch = format!("{pearl_id}-{slug}");
        let path = project.parent().unwrap_or(project).join(format!("{repo}-{branch}"));
        if path.exists() {
            return Ok(path);
        }
        let path_s = path.to_string_lossy().into_owned();
        git(project, &["worktree", "add", &path_s, "-b", &branch, base])?;
        Ok(path)
    }

    /// `flow.new`: resolve the worktree, launch under tmux, persist, broadcast.
    ///
    /// # Errors
    /// When the worktree can't be resolved/created or tmux refuses.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new_session(&self, req: NewRequest) -> Result<Session> {
        let project = req
            .project
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| req.worktree.as_deref().map(|w| project_root(Path::new(w))))
            .unwrap_or_else(|| self.inner.default_project.clone());
        let worktree = match (&req.worktree, &req.pearl_id) {
            (Some(w), _) => PathBuf::from(w),
            (None, Some(pearl)) => {
                let slug = slugify(req.title.as_deref().or(req.prompt.as_deref()).unwrap_or("work"), 24);
                let slug = if slug.is_empty() { "work".to_string() } else { slug };
                Self::create_worktree(&project, pearl, &slug, "HEAD")?
            }
            (None, None) => project.clone(),
        };
        if !worktree.is_dir() {
            bail!("worktree does not exist: {}", worktree.display());
        }
        let agent_session_id = match req.kind {
            SessionKind::Claude => Some(uuid::Uuid::new_v4().to_string()),
            _ => None,
        };
        let argv = req
            .argv
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| default_argv(req.kind, agent_session_id.as_deref(), req.model.as_deref(), req.prompt.as_deref()));
        let branch = git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
        let title = req
            .title
            .clone()
            .unwrap_or_else(|| default_title(req.kind, req.prompt.as_deref(), req.pearl_id.as_deref(), &worktree));
        let session = self.with_store(|st| {
            st.create(NewSession {
                kind: Some(req.kind),
                title,
                project: project.to_string_lossy().into_owned(),
                worktree: worktree.to_string_lossy().into_owned(),
                branch,
                pearl_id: req.pearl_id.clone(),
                agent_session_id: agent_session_id.clone(),
                argv: argv.clone(),
                tmux_session: None,
                fan_out_id: req.fan_out_id.clone(),
            })
        })?;
        let session = self.launch(&session, &argv)?;
        // A shell has no hooks and nothing to scrape — it is simply ready.
        if !session.kind.is_agent() {
            return Ok(self.set_state(&session.id, SessionState::Idle, None)?.unwrap_or(session));
        }
        Ok(session)
    }

    /// Launch `argv` in the session's tmux session (named after the id),
    /// record pid + start time, broadcast.
    fn launch(&self, session: &Session, argv: &[String]) -> Result<Session> {
        let tmux_name = session.id.clone();
        if tmux::session_alive(&tmux_name) {
            tmux::kill_session(&tmux_name);
        }
        let pid = tmux::launch(&tmux_name, Path::new(&session.worktree), argv)?;
        let start = proc::start_time(pid);
        self.with_store(|st| st.set_process(&session.id, Some(&tmux_name), Some(pid), start, argv))?;
        if let Some(agent) = &session.agent_session_id {
            self.rt().claims.insert(agent.clone(), (pid, Instant::now()));
        }
        let s = self.require(&session.id)?;
        self.emit_session(&s);
        Ok(s)
    }

    fn pty_for(&self, id: &str, cols: u16, rows: u16) -> Result<Arc<PtyAttach>> {
        if let Some(p) = self.inner.ptys.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id) {
            return Ok(p.clone());
        }
        let session = self.require(id)?;
        let tmux_name = session.tmux_session.ok_or_else(|| anyhow!("session {id} has no tmux session"))?;
        if !tmux::session_alive(&tmux_name) {
            bail!("session {id} is not running");
        }
        let sid = id.to_string();
        let weak = Arc::downgrade(&self.inner);
        let on_output: OnOutput = Arc::new(move |seq, bytes| {
            let Some(inner) = weak.upgrade() else { return };
            if bytes.is_empty() {
                // EOF — the attach client died (session killed / detached).
                inner.ptys.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&sid);
                return;
            }
            let _ = inner.tx.send(ServerFrame::Output {
                id: sid.clone(),
                seq,
                data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            });
        });
        let pty = PtyAttach::spawn(&tmux::attach_argv(&tmux_name), cols, rows, on_output)?;
        self.inner
            .ptys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string(), pty.clone());
        Ok(pty)
    }

    /// `flow.attach`: ensure a PTY client exists and size it.
    ///
    /// # Errors
    /// When the session is unknown or not running.
    pub fn attach(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        let pty = self.pty_for(id, cols, rows)?;
        pty.clients.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        pty.resize(cols, rows)
    }

    /// `flow.detach`: drop the PTY client when the last flow client leaves.
    pub fn detach(&self, id: &str) {
        let mut ptys = self.inner.ptys.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(p) = ptys.get(id) {
            let left = p.clients.fetch_sub(1, std::sync::atomic::Ordering::Relaxed).saturating_sub(1);
            if left == 0 {
                if let Some(p) = ptys.remove(id) {
                    p.close();
                }
            }
        }
    }

    /// `flow.input`: raw bytes to the PTY (created on demand at the pane's size).
    ///
    /// # Errors
    /// When the session is unknown/not running or the PTY is closed.
    pub fn input(&self, id: &str, data: &[u8]) -> Result<()> {
        let (cols, rows) = self
            .require(id)?
            .tmux_session
            .as_deref()
            .and_then(|t| tmux::pane_size(t).ok())
            .unwrap_or((120, 40));
        self.pty_for(id, cols, rows)?.write(data)
    }

    /// `flow.resize`.
    ///
    /// # Errors
    /// When the session is unknown/not running.
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        self.pty_for(id, cols, rows)?.resize(cols, rows)
    }

    /// `flow.send`: text + Enter via bracketed paste (steering, not raw bytes).
    ///
    /// # Errors
    /// When the session is unknown or tmux refuses.
    pub fn send(&self, id: &str, text: &str) -> Result<()> {
        let s = self.require(id)?;
        let t = s.tmux_session.as_deref().ok_or_else(|| anyhow!("session {id} has no tmux session"))?;
        tmux::send_text(t, text)
    }

    /// `flow.snapshot`: plain-text visible pane.
    ///
    /// # Errors
    /// When the session is unknown or tmux refuses.
    pub fn snapshot(&self, id: &str) -> Result<ServerFrame> {
        let s = self.require(id)?;
        let t = s.tmux_session.as_deref().ok_or_else(|| anyhow!("session {id} has no tmux session"))?;
        let (cols, rows) = tmux::pane_size(t)?;
        Ok(ServerFrame::Screen {
            id: id.to_string(),
            cols,
            rows,
            text: tmux::capture_visible(t)?,
        })
    }

    /// `flow.mark_read`.
    ///
    /// # Errors
    /// On a store failure.
    pub fn mark_read(&self, id: &str) -> Result<()> {
        self.with_store(|st| st.set_unread(id, false))?;
        if let Some(s) = self.get(id)? {
            self.emit_session(&s);
        }
        Ok(())
    }

    /// `flow.approve`: answer a pending hook request, or press the key on a
    /// scraped approval menu.
    ///
    /// # Errors
    /// When the session is unknown or the keystroke can't be sent.
    pub fn approve(&self, id: &str, request_id: &str, decision: Decision) -> Result<()> {
        let s = self.require(id)?;
        let pending = self
            .inner
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(request_id)
            .filter(|p| p.session_id == id)
            .and_then(|p| p.tx.take());
        if let Some(tx) = pending {
            let _ = tx.send(decision);
        } else {
            let t = s.tmux_session.as_deref().ok_or_else(|| anyhow!("session {id} has no tmux session"))?;
            tmux::send_key(t, approval_keystroke(decision))?;
        }
        self.set_state(id, SessionState::Working, None)?;
        Ok(())
    }

    /// `flow.kill`: kill the process tree; optionally relaunch with `--resume`.
    ///
    /// # Errors
    /// When the session is unknown or the relaunch fails.
    pub fn kill(&self, id: &str, resume: bool) -> Result<Session> {
        let s = self.require(id)?;
        let tmux_name = s.tmux_session.clone();
        if let Some(pid) = s.pid {
            if proc::is_alive(pid, s.pid_start) {
                proc::kill_tree(pid, KILL_GRACE);
            }
        }
        let exit = tmux_name.as_deref().and_then(|t| tmux::pane_exit_status(t).ok().flatten());
        if let Some(t) = &tmux_name {
            tmux::kill_session(t);
        }
        self.drop_pty(id);
        self.with_store(|st| st.set_exit_code(id, exit))?;
        if resume {
            self.rt().resume_attempts.remove(id);
            return self.relaunch(&self.require(id)?);
        }
        self.set_state(id, SessionState::Done, None)?.ok_or_else(|| anyhow!("no such session: {id}"))
    }

    fn drop_pty(&self, id: &str) {
        let removed = self.inner.ptys.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
        if let Some(p) = removed {
            p.close();
        }
    }

    /// Remove a session row (terminal sessions only) and broadcast.
    ///
    /// # Errors
    /// When the session is live.
    pub fn remove(&self, id: &str) -> Result<()> {
        let s = self.require(id)?;
        if !s.state.is_terminal() {
            bail!("session {id} is {} — kill it first", s.state);
        }
        if let Some(t) = &s.tmux_session {
            tmux::kill_session(t);
        }
        self.with_store(|st| st.remove(id))?;
        self.emit(ServerFrame::SessionRemoved { id: id.to_string() });
        Ok(())
    }

    /// Relaunch a dead agent with the resume argv, honouring rule 4.
    fn relaunch(&self, s: &Session) -> Result<Session> {
        if let Some(agent) = &s.agent_session_id {
            let holder = {
                let rt = self.rt();
                claim_holder(&rt.claims, agent, Instant::now(), |pid| pid != s.pid.unwrap_or(0) && proc::is_alive(pid, None))
            };
            // Another row owning the same harness session with a live pid
            // counts too (the claims map is per daemon process).
            let holder = holder.or_else(|| {
                self.list()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|o| o.id != s.id && o.agent_session_id.as_deref() == Some(agent) && !o.state.is_terminal())
                    .find_map(|o| o.pid.filter(|p| proc::is_alive(*p, o.pid_start)))
            });
            if let Some(pid) = holder {
                let att = Attention::new("held").with_detail(format!("session {agent} is owned by live pid {pid}"));
                return self
                    .set_state(&s.id, SessionState::NeedsYou, Some(att))?
                    .ok_or_else(|| anyhow!("no such session"));
            }
        }
        let argv = resume_argv(s);
        let launched = self.launch(s, &argv)?;
        self.set_state(&launched.id, SessionState::Starting, None)?
            .ok_or_else(|| anyhow!("no such session"))
    }

    // ── hooks ─────────────────────────────────────────────────────────────

    /// `POST /api/flow/hooks`: map the event to state; a `PermissionRequest`
    /// returns [`HookReply::Pending`] for the host to long-poll.
    ///
    /// # Errors
    /// On a store failure. An unknown `session_id` is NOT an error (the
    /// hook script must never block the harness) — it returns `Immediate({})`.
    pub fn hook(&self, ev: HookEvent) -> Result<HookReply> {
        let Some(s) = self.with_store(|st| st.get_by_agent_session(&ev.session_id))? else {
            tracing::debug!(session = %ev.session_id, event = %ev.event, "flow hook for an unknown session");
            return Ok(HookReply::Immediate(json!({})));
        };
        self.rt().hook_seen.insert(s.id.clone());
        match map_hook_event(&ev.event, &ev.payload) {
            HookOutcome::Working => {
                self.set_state(&s.id, SessionState::Working, None)?;
            }
            HookOutcome::Idle => {
                self.with_store(|st| st.set_unread(&s.id, true))?;
                self.set_state(&s.id, SessionState::Idle, None)?;
            }
            HookOutcome::NeedsYou(mut att) => {
                if ev.event == "PermissionRequest" {
                    let request_id = uuid::Uuid::new_v4().simple().to_string();
                    att.request_id = Some(request_id.clone());
                    let (tx, rx) = oneshot::channel();
                    self.inner.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(
                        request_id.clone(),
                        PendingApproval {
                            session_id: s.id.clone(),
                            tx: Some(tx),
                        },
                    );
                    self.set_state(&s.id, SessionState::NeedsYou, Some(att))?;
                    return Ok(HookReply::Pending {
                        request_id,
                        rx,
                        payload: ev.payload,
                    });
                }
                // A Notification while a hook request is pending is the same
                // prompt seen twice — keep the request_id.
                if s.state != SessionState::NeedsYou {
                    self.set_state(&s.id, SessionState::NeedsYou, Some(att))?;
                }
            }
            HookOutcome::Ended | HookOutcome::None => {}
        }
        Ok(HookReply::Immediate(json!({})))
    }

    /// Finish a pending permission request (called by the host after the
    /// long-poll resolves or times out). Returns the harness reply body.
    #[must_use]
    pub fn finish_pending(&self, request_id: &str, decision: Option<Decision>, payload: &Value) -> Value {
        self.inner.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(request_id);
        decision.map_or_else(|| json!({}), |d| permission_reply(d, payload))
    }

    /// True while a hook-reported permission request awaits `flow.approve`.
    #[must_use]
    pub fn has_pending(&self, session_id: &str) -> bool {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|p| p.session_id == session_id && p.tx.is_some())
    }

    // ── supervision ────────────────────────────────────────────────────────

    /// One supervision pass over every live session. Cheap when nothing
    /// changed; safe to call every couple of seconds.
    ///
    /// # Errors
    /// On a store failure (per-session tmux/ps errors are logged, not raised).
    pub fn supervise_tick(&self) -> Result<()> {
        let now = Utc::now();
        for s in self.with_store(FlowStore::list_live)? {
            if let Err(e) = self.supervise_one(&s, now) {
                tracing::warn!(session = %s.id, error = %e, "flow supervision");
            }
        }
        Ok(())
    }

    fn supervise_one(&self, s: &Session, now: DateTime<Utc>) -> Result<()> {
        // A relaunch is scheduled (rule 2 backoff) — fire it when due.
        let due = self.rt().relaunch_at.get(&s.id).copied();
        if let Some(at) = due {
            if Instant::now() >= at {
                self.rt().relaunch_at.remove(&s.id);
                self.relaunch(s)?;
            }
            return Ok(());
        }
        let Some(t) = s.tmux_session.as_deref() else { return Ok(()) };
        if !tmux::session_alive(t) {
            return self.on_death(s, None);
        }
        if let Some(code) = tmux::pane_exit_status(t)? {
            self.with_store(|st| st.set_exit_code(&s.id, Some(code)))?;
            tmux::kill_session(t);
            self.drop_pty(&s.id);
            if code == 0 {
                // Rule 5: exit 0 is proven — the PTY reported it.
                self.set_state(&s.id, SessionState::Done, None)?;
                return Ok(());
            }
            return self.on_death(s, Some(code));
        }
        if !s.kind.is_agent() {
            return Ok(());
        }
        // Usage-limit resume due?
        if s.state == SessionState::Limited {
            let due = s.attention.as_ref().and_then(|a| a.resume_at).is_some_and(|at| now >= at);
            if due {
                tracing::info!(session = %s.id, "flow: usage limit window passed — resuming");
                self.rt().limit_resumed_at.insert(s.id.clone(), Instant::now());
                tmux::send_key(t, "Enter")?;
                self.set_state(&s.id, SessionState::Working, None)?;
            }
            return Ok(());
        }
        // Scrape the visible pane: limits always; approvals when hooks
        // didn't report one; working/idle only when hooks never spoke.
        let pane = tmux::capture_visible(t)?;
        let hooks_seen = self.rt().hook_seen.contains(&s.id);
        match detect_state(&pane) {
            PaneState::UsageLimit => {
                let recently_resumed = self.rt().limit_resumed_at.get(&s.id).is_some_and(|at| at.elapsed() < LIMIT_REARM_GRACE);
                if !recently_resumed {
                    let at = limit::resume_at(&pane, now);
                    let mut att = Attention::new("usage_limit").with_detail(format!("resumes at {}", at.to_rfc3339()));
                    att.resume_at = Some(at);
                    self.set_state(&s.id, SessionState::Limited, Some(att))?;
                }
            }
            PaneState::AwaitingApproval => {
                if s.state != SessionState::NeedsYou && !self.has_pending(&s.id) {
                    let mut att = Attention::new("permission").with_detail("approval prompt on screen");
                    att.request_id = Some(format!("scrape-{}", uuid::Uuid::new_v4().simple()));
                    self.set_state(&s.id, SessionState::NeedsYou, Some(att))?;
                }
            }
            PaneState::Working if !hooks_seen && s.state != SessionState::Working => {
                self.set_state(&s.id, SessionState::Working, None)?;
            }
            PaneState::Idle if !hooks_seen && matches!(s.state, SessionState::Starting | SessionState::Working) => {
                if s.state == SessionState::Working {
                    self.with_store(|st| st.set_unread(&s.id, true))?;
                }
                self.set_state(&s.id, SessionState::Idle, None)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Rule 2: unexpected death → schedule a resume with backoff, up to
    /// [`MAX_RESUME_ATTEMPTS`], then `dead` (attention `crashed`).
    fn on_death(&self, s: &Session, code: Option<i32>) -> Result<()> {
        self.drop_pty(&s.id);
        let detail_exit = code.map_or_else(|| "process vanished".to_string(), |c| format!("exit {c}"));
        if !s.kind.is_agent() {
            let state = if code == Some(0) { SessionState::Done } else { SessionState::Dead };
            let att = (state == SessionState::Dead).then(|| Attention::new("crashed").with_detail(detail_exit));
            self.set_state(&s.id, state, att)?;
            return Ok(());
        }
        let attempt = {
            let mut rt = self.rt();
            let n = rt.resume_attempts.entry(s.id.clone()).or_insert(0);
            *n += 1;
            *n
        };
        if attempt > MAX_RESUME_ATTEMPTS {
            let att = Attention::new("crashed").with_detail(format!("{detail_exit}; gave up after {MAX_RESUME_ATTEMPTS} resumes"));
            self.set_state(&s.id, SessionState::Dead, Some(att))?;
            return Ok(());
        }
        let wait = resume_backoff(attempt - 1);
        self.rt().relaunch_at.insert(s.id.clone(), Instant::now() + wait);
        let mut att = Attention::new("crashed").with_detail(format!(
            "{detail_exit}; resuming in {}s (attempt {attempt}/{MAX_RESUME_ATTEMPTS})",
            wait.as_secs()
        ));
        att.resume_at = Some(Utc::now() + chrono::Duration::from_std(wait).unwrap_or_else(|_| chrono::Duration::seconds(5)));
        self.set_state(&s.id, SessionState::Starting, Some(att))?;
        Ok(())
    }

    // ── fan-out ────────────────────────────────────────────────────────────

    /// `flow.fanout.new`: N worktrees + N sessions + N child pearls.
    ///
    /// # Errors
    /// When the project isn't a git repo or any candidate fails to launch.
    pub fn fanout_new(&self, prompt: &str, pearl_id: &str, candidates: &[CandidateSpec], project: Option<&str>) -> Result<(FanOut, Vec<Session>)> {
        if candidates.is_empty() {
            bail!("a fan-out needs at least one candidate");
        }
        let project = project.map_or_else(|| self.inner.default_project.clone(), PathBuf::from);
        let base = git(&project, &["rev-parse", "HEAD"])?;
        let fo = self.with_store(|st| st.create_fan_out(prompt, &base, pearl_id))?;
        let mut sessions = Vec::new();
        for c in candidates {
            let label = slugify(&c.label, 24);
            let label = if label.is_empty() { "cand".to_string() } else { label };
            let worktree = Self::create_worktree(&project, pearl_id, &label, &base)?;
            let child = create_child_pearl(&project, pearl_id, &fo.id, &c.label, prompt).ok();
            let s = self.new_session(NewRequest {
                kind: c.kind,
                worktree: Some(worktree.to_string_lossy().into_owned()),
                project: Some(project.to_string_lossy().into_owned()),
                pearl_id: child.clone().or_else(|| Some(pearl_id.to_string())),
                prompt: Some(prompt.to_string()),
                argv: None,
                title: Some(format!("{pearl_id} · {}", c.label)),
                model: c.model.clone(),
                fan_out_id: Some(fo.id.clone()),
            })?;
            sessions.push(s);
        }
        self.emit(ServerFrame::Fanout {
            fan_out: fo.clone(),
            candidates: sessions.clone(),
        });
        Ok((fo, sessions))
    }

    /// `flow.fanout.pick`: merge the winner (`th worktree merge` semantics),
    /// GC the losers' worktrees + branches, close their child pearls, keep
    /// every transcript (session rows stay).
    ///
    /// # Errors
    /// When the fan-out/winner is unknown or the merge fails.
    pub fn fanout_pick(&self, fan_out_id: &str, winner_session_id: &str) -> Result<(FanOut, Vec<Session>)> {
        let fo = self
            .with_store(|st| st.get_fan_out(fan_out_id))?
            .ok_or_else(|| anyhow!("no such fan-out: {fan_out_id}"))?;
        let cands = self.with_store(|st| st.list_by_fan_out(fan_out_id))?;
        let winner = cands
            .iter()
            .find(|c| c.id == winner_session_id)
            .ok_or_else(|| anyhow!("{winner_session_id} is not a candidate of {fan_out_id}"))?;
        let project = Path::new(&winner.project);
        let branch = winner.branch.clone().ok_or_else(|| anyhow!("winner has no branch"))?;
        // Same steps as `th worktree merge`, run in the main checkout.
        git(project, &["checkout", "main"])?;
        if let Err(e) = git(project, &["pull", "--rebase"]) {
            tracing::warn!(error = %e, "fan-out pick: pull --rebase failed; merging anyway");
        }
        git(
            project,
            &["merge", &branch, "--no-ff", "-m", &format!("{}: merge fan-out winner {branch}", fo.pearl_id)],
        )?;
        self.with_store(|st| st.set_fan_out_winner(fan_out_id, winner_session_id))?;
        let mut closed = Vec::new();
        for c in &cands {
            if c.id == winner.id {
                continue;
            }
            if !c.state.is_terminal() {
                let _ = self.kill(&c.id, false);
            }
            let _ = git(project, &["worktree", "remove", "--force", &c.worktree]);
            if let Some(b) = &c.branch {
                let _ = git(project, &["branch", "-D", b]);
            }
            if let Some(p) = c.pearl_id.as_deref().filter(|p| *p != fo.pearl_id) {
                closed.push(p.to_string());
            }
        }
        if let Some(p) = winner.pearl_id.as_deref().filter(|p| *p != fo.pearl_id) {
            closed.push(p.to_string());
        }
        if !closed.is_empty() {
            let mut args = vec!["pearls", "close"];
            args.extend(closed.iter().map(String::as_str));
            if let Err(e) = th(project, &args) {
                tracing::warn!(error = %e, "fan-out pick: closing child pearls failed");
            }
        }
        let fo = self.with_store(|st| st.get_fan_out(fan_out_id))?.unwrap_or(fo);
        let cands = self.with_store(|st| st.list_by_fan_out(fan_out_id))?;
        self.emit(ServerFrame::Fanout {
            fan_out: fo.clone(),
            candidates: cands.clone(),
        });
        Ok((fo, cands))
    }

    // ── pearl rail ─────────────────────────────────────────────────────────

    /// `GET /api/flow/sessions/{id}/handoff` — git facts from the engine,
    /// pearl/handoff/checkpoints from `th pearls` (degrading to nulls when
    /// the installed `th` predates `--json`).
    ///
    /// # Errors
    /// When the session is unknown.
    pub fn handoff(&self, id: &str) -> Result<Value> {
        let s = self.require(id)?;
        let wt = Path::new(&s.worktree);
        let head = git(wt, &["rev-parse", "HEAD"]).ok();
        let dirty: Vec<String> = git(wt, &["status", "--porcelain"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.get(3..).map(str::to_string))
            .collect();
        let pearl_json = s.pearl_id.as_deref().and_then(|p| {
            th(Path::new(&s.project), &["pearls", "show", p, "--json"])
                .ok()
                .and_then(|out| serde_json::from_str::<Value>(&out).ok())
                .or_else(|| {
                    th(Path::new(&s.project), &["pearls", "show", p])
                        .ok()
                        .map(|text| json!({ "id": p, "text": text }))
                })
        });
        let pr = s.branch.as_deref().and_then(|b| pr_for_branch(wt, b));
        Ok(json!({
            "pearl": pearl_json.clone().unwrap_or(Value::Null),
            "handoff": {
                "worktree": s.worktree,
                "branch": s.branch,
                "head": head,
                "dirty": dirty,
                "agent_session_id": s.agent_session_id,
                "next": pearl_json.as_ref().and_then(|p| p.pointer("/handoff/next").cloned()).unwrap_or(Value::Null),
            },
            "checkpoints": pearl_json.as_ref().and_then(|p| p.get("checkpoints").cloned()).unwrap_or_else(|| json!([])),
            "blocks": pearl_json.as_ref().and_then(|p| p.get("blocks").or_else(|| p.get("blocked_by")).cloned()).unwrap_or_else(|| json!([])),
            "pr": pr.unwrap_or(Value::Null),
        }))
    }
}

/// `th pearls create` a child pearl in the main checkout; returns its id.
fn create_child_pearl(project: &Path, parent: &str, fan_out_id: &str, label: &str, prompt: &str) -> Result<String> {
    let short: String = prompt.chars().take(60).collect();
    let title = format!("{parent} candidate {label}: {short}");
    let desc = format!("Fan-out candidate `{label}` of {parent} (fan-out {fan_out_id}).\n\nPrompt:\n{prompt}");
    let out = th(
        project,
        &[
            "pearls",
            "create",
            "--title",
            &title,
            "--description",
            &desc,
            "--type",
            "task",
            "--priority",
            "3",
            "--label",
            "fanout",
        ],
    )?;
    parse_pearl_id(&out).ok_or_else(|| anyhow!("no pearl id in `th pearls create` output: {out}"))
}

/// The first `xx-hhhhhh` pearl id in `text`.
#[must_use]
pub fn parse_pearl_id(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"\b([a-z]{1,8}-[0-9a-f]{6})\b").ok()?;
    re.captures(text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// `{number, url, ci}` for the open PR on `branch`, via `gh` (None when gh
/// is absent, unauthenticated, or there is no PR).
fn pr_for_branch(cwd: &Path, branch: &str) -> Option<Value> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--limit",
            "1",
            "--json",
            "number,url,statusCheckRollup",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let pr = v.as_array()?.first()?;
    Some(json!({
        "number": pr.get("number"),
        "url": pr.get("url"),
        "ci": ci_rollup(pr.get("statusCheckRollup")),
    }))
}

/// Collapse gh's per-check rollup into `success` | `failure` | `pending` | null.
#[must_use]
pub fn ci_rollup(rollup: Option<&Value>) -> Value {
    let Some(checks) = rollup.and_then(Value::as_array) else { return Value::Null };
    if checks.is_empty() {
        return Value::Null;
    }
    let mut pending = false;
    for c in checks {
        let s = c
            .get("conclusion")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| c.get("state").and_then(Value::as_str))
            .unwrap_or("")
            .to_uppercase();
        match s.as_str() {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" => return json!("failure"),
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            _ => pending = true,
        }
    }
    json!(if pending { "pending" } else { "success" })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn engine(tmp: &Path) -> Engine {
        Engine::open(EngineConfig {
            db_path: tmp.join("flow.db"),
            default_project: tmp.to_path_buf(),
            version: "t".into(),
            machine_label: "m".into(),
        })
        .unwrap()
    }

    #[test]
    fn slug_and_title_and_argv_helpers() {
        assert_eq!(slugify("Fix the Auth bug!!", 24), "fix-the-auth-bug");
        assert_eq!(slugify("   ", 24), "");
        assert!(slugify(&"x".repeat(100), 10).len() <= 10);
        assert_eq!(default_title(SessionKind::Claude, Some("  do it  "), None, Path::new("/a/b")), "do it");
        assert_eq!(default_title(SessionKind::Claude, None, Some("th-1"), Path::new("/a/b")), "th-1");
        assert_eq!(default_title(SessionKind::Shell, None, None, Path::new("/a/b")), "shell · b");
        assert_eq!(
            default_argv(SessionKind::Claude, Some("u"), Some("opus"), Some("hi")),
            vec!["claude", "--session-id", "u", "--model", "opus", "hi"]
        );
        assert_eq!(default_argv(SessionKind::Claude, None, None, Some("  ")), vec!["claude"]);
        assert_eq!(default_argv(SessionKind::Codex, None, None, Some("p")), vec!["codex", "p"]);
        assert_eq!(default_argv(SessionKind::Shell, None, None, None)[1], "-l");
    }

    #[test]
    fn resume_argv_and_backoff() {
        let mut s = crate::store::FlowStore::open_in_memory()
            .unwrap()
            .create(NewSession {
                kind: Some(SessionKind::Claude),
                agent_session_id: Some("u".into()),
                argv: vec!["claude".into(), "--session-id".into(), "u".into()],
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(resume_argv(&s), vec!["claude", "--resume", "u"]);
        s.kind = SessionKind::Codex;
        assert_eq!(resume_argv(&s), s.argv);
        assert_eq!(resume_backoff(0), Duration::from_secs(5));
        assert_eq!(resume_backoff(1), Duration::from_secs(10));
        assert_eq!(resume_backoff(2), Duration::from_secs(20));
        assert!(resume_backoff(40) > Duration::from_secs(20), "saturates, never panics");
    }

    #[test]
    fn duplicate_resume_guard_is_pure_and_time_boxed() {
        let mut claims = HashMap::new();
        let now = Instant::now();
        claims.insert("u".to_string(), (4242u32, now));
        assert_eq!(claim_holder(&claims, "u", now, |_| true), Some(4242));
        assert_eq!(claim_holder(&claims, "u", now, |_| false), None, "dead holder releases the claim");
        assert_eq!(claim_holder(&claims, "other", now, |_| true), None);
        let later = now + RESUME_CLAIM_TTL + Duration::from_secs(1);
        assert_eq!(claim_holder(&claims, "u", later, |_| true), None, "claims expire after 60 s");
    }

    #[test]
    fn pearl_id_parse_and_ci_rollup() {
        assert_eq!(parse_pearl_id("Created th-abc123: title").as_deref(), Some("th-abc123"));
        assert_eq!(parse_pearl_id("  ● smooai-1a2b3c ").as_deref(), Some("smooai-1a2b3c"));
        assert!(parse_pearl_id("nothing here").is_none());
        assert_eq!(ci_rollup(None), Value::Null);
        assert_eq!(ci_rollup(Some(&json!([]))), Value::Null);
        assert_eq!(ci_rollup(Some(&json!([{"conclusion":"SUCCESS"},{"conclusion":"SKIPPED"}]))), json!("success"));
        assert_eq!(ci_rollup(Some(&json!([{"conclusion":"SUCCESS"},{"conclusion":"FAILURE"}]))), json!("failure"));
        assert_eq!(ci_rollup(Some(&json!([{"conclusion":"","state":"IN_PROGRESS"}]))), json!("pending"));
    }

    #[test]
    fn project_root_outside_git_is_the_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // tempdir may itself be inside a git checkout on some machines; use a
        // nested dir and only assert the "no repo ⇒ self" branch when true.
        let d = tmp.path().join("plain");
        std::fs::create_dir_all(&d).unwrap();
        let root = project_root(&d);
        assert!(root == d || root.join(".git").exists());
    }

    #[test]
    fn hook_for_unknown_session_is_a_quiet_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let r = e
            .hook(HookEvent {
                harness: "claude-code".into(),
                event: "Stop".into(),
                session_id: "nope".into(),
                cwd: None,
                payload: json!({}),
            })
            .unwrap();
        assert!(matches!(r, HookReply::Immediate(v) if v == json!({})));
    }

    #[test]
    fn hooks_drive_state_and_permission_long_poll() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Claude),
                    agent_session_id: Some("uuid-1".into()),
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let mut rx = e.subscribe();
        let ev = |event: &str, payload: Value| HookEvent {
            harness: "claude-code".into(),
            event: event.into(),
            session_id: "uuid-1".into(),
            cwd: None,
            payload,
        };
        e.hook(ev("UserPromptSubmit", json!({}))).unwrap();
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Working);
        assert!(matches!(rx.try_recv().unwrap(), ServerFrame::Session { .. }));

        e.hook(ev("Stop", json!({}))).unwrap();
        let after = e.get(&s.id).unwrap().unwrap();
        assert_eq!(after.state, SessionState::Idle);
        assert!(after.unread, "Stop marks unread");
        e.mark_read(&s.id).unwrap();
        assert!(!e.get(&s.id).unwrap().unwrap().unread);

        let reply = e
            .hook(ev("PermissionRequest", json!({"tool_name":"Bash","tool_input":{"command":"ls"}})))
            .unwrap();
        let HookReply::Pending {
            request_id,
            rx: decision_rx,
            payload,
        } = reply
        else {
            panic!("expected pending")
        };
        let now = e.get(&s.id).unwrap().unwrap();
        assert_eq!(now.state, SessionState::NeedsYou);
        assert_eq!(now.attention.as_ref().unwrap().request_id.as_deref(), Some(request_id.as_str()));
        assert_eq!(now.attention.as_ref().unwrap().detail.as_deref(), Some("Bash: ls"));
        assert!(e.has_pending(&s.id));

        // A Notification for the same prompt keeps the request id.
        e.hook(ev(
            "Notification",
            json!({"notification_type":"permission_prompt","message":"needs permission"}),
        ))
        .unwrap();
        assert_eq!(
            e.get(&s.id).unwrap().unwrap().attention.unwrap().request_id.as_deref(),
            Some(request_id.as_str())
        );

        // No tmux session exists, so approve must resolve via the pending
        // channel (not a keystroke) — that's the hook path.
        e.approve(&s.id, &request_id, Decision::AllowSession).unwrap();
        let d = decision_rx.blocking_recv().unwrap();
        assert_eq!(d, Decision::AllowSession);
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Working);
        let body = e.finish_pending(&request_id, Some(d), &payload);
        assert_eq!(body["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert!(!e.has_pending(&s.id));
        // Timeout path replies `{}`.
        assert_eq!(e.finish_pending("gone", None, &payload), json!({}));

        // Ended is a no-op for state (the PTY decides done/dead).
        e.hook(ev("SessionEnd", json!({}))).unwrap();
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Working);

        // Second approve on the same request id falls through to the
        // keystroke path, which needs tmux — an error, not a panic.
        assert!(e.approve(&s.id, &request_id, Decision::Allow).is_err());
    }

    #[test]
    fn on_death_backs_off_then_gives_up_and_shells_die_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let mk = |kind: SessionKind| {
            e.with_store(|st| {
                st.create(NewSession {
                    kind: Some(kind),
                    agent_session_id: (kind == SessionKind::Claude).then(|| "u".to_string()),
                    argv: vec!["x".into()],
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap()
        };
        let agent = mk(SessionKind::Claude);
        for attempt in 1..=MAX_RESUME_ATTEMPTS {
            e.on_death(&agent, Some(137)).unwrap();
            let s = e.get(&agent.id).unwrap().unwrap();
            assert_eq!(s.state, SessionState::Starting);
            let att = s.attention.unwrap();
            assert_eq!(att.reason, "crashed");
            assert!(att.detail.unwrap().contains(&format!("attempt {attempt}/")));
            assert!(att.resume_at.is_some());
            assert!(e.rt().relaunch_at.contains_key(&agent.id));
            e.rt().relaunch_at.remove(&agent.id);
        }
        e.on_death(&agent, None).unwrap();
        let s = e.get(&agent.id).unwrap().unwrap();
        assert_eq!(s.state, SessionState::Dead);
        assert!(s.attention.unwrap().detail.unwrap().contains("gave up"));

        let shell = mk(SessionKind::Shell);
        e.on_death(&shell, Some(0)).unwrap();
        assert_eq!(e.get(&shell.id).unwrap().unwrap().state, SessionState::Done);
        let shell2 = mk(SessionKind::Shell);
        e.on_death(&shell2, Some(1)).unwrap();
        let s = e.get(&shell2.id).unwrap().unwrap();
        assert_eq!(s.state, SessionState::Dead);
        assert_eq!(s.attention.unwrap().reason, "crashed");
    }

    #[test]
    #[cfg(unix)]
    fn relaunch_refuses_when_another_live_row_owns_the_harness_session() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let mk = || {
            e.with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Claude),
                    agent_session_id: Some("shared".into()),
                    argv: vec!["claude".into()],
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap()
        };
        let holder = mk();
        let me = std::process::id();
        e.with_store(|st| st.set_process(&holder.id, Some("x"), Some(me), proc::start_time(me), &["claude".into()]))
            .unwrap();
        let victim = mk();
        let out = e.relaunch(&victim).unwrap();
        assert_eq!(out.state, SessionState::NeedsYou);
        let att = out.attention.unwrap();
        assert_eq!(att.reason, "held");
        assert!(att.detail.unwrap().contains(&me.to_string()));
    }

    #[test]
    fn remove_refuses_live_and_broadcasts_for_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Shell),
                    project: "/p".into(),
                    worktree: "/p".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        assert!(e.remove(&s.id).is_err());
        e.set_state(&s.id, SessionState::Done, None).unwrap();
        let mut rx = e.subscribe();
        e.remove(&s.id).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), ServerFrame::SessionRemoved { id } if id == s.id));
        assert!(e.get(&s.id).unwrap().is_none());
        assert!(e.hello().unwrap().to_wire().contains("\"sessions\":[]"));
    }

    #[test]
    fn handoff_degrades_without_th_or_gh() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let e = engine(tmp.path());
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Claude),
                    pearl_id: Some("th-abc123".into()),
                    agent_session_id: Some("u".into()),
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let h = e.handoff(&s.id).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        assert!(h["pearl"].is_null());
        assert_eq!(h["handoff"]["agent_session_id"], "u");
        assert_eq!(h["handoff"]["worktree"].as_str().unwrap(), tmp.path().to_string_lossy());
        assert!(h["checkpoints"].as_array().unwrap().is_empty());
        assert!(h["blocks"].as_array().unwrap().is_empty());
        assert!(h["pr"].is_null());
    }

    /// Live: launch a real `shell`-kind session (a `cat` loop so it never
    /// exits), stream bytes through the PTY, send text, snapshot, kill.
    /// Skips when tmux is missing.
    #[test]
    fn live_shell_session_streams_and_dies_cleanly() {
        if !tmux::tmux_available() {
            eprintln!("skipping: tmux not available");
            return;
        }
        let _g = crate::tmux::tests_env_lock();
        let sock = format!("flow-e-{}", std::process::id());
        std::env::set_var("SMOOTH_FLOW_TMUX_SOCKET", &sock);
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let mut rx = e.subscribe();
        let s = e
            .new_session(NewRequest {
                kind: SessionKind::Shell,
                worktree: Some(tmp.path().to_string_lossy().into()),
                argv: Some(vec!["sh".into(), "-c".into(), "echo FLOW-READY; cat".into()]),
                title: Some("t".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert!(s.pid.is_some() && s.tmux_session.as_deref() == Some(s.id.as_str()));
        assert!(proc::is_alive(s.pid.unwrap(), s.pid_start));

        e.attach(&s.id, 100, 30).unwrap();
        e.send(&s.id, "hello-flow").unwrap();
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !seen.contains("hello-flow") {
            match rx.try_recv() {
                Ok(ServerFrame::Output { id, data_b64, .. }) if id == s.id => {
                    seen.push_str(&String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(data_b64).unwrap()));
                }
                Err(broadcast::error::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
                Ok(_) | Err(_) => {}
            }
        }
        assert!(seen.contains("hello-flow"), "PTY stream: {seen:?}");
        // Raw input path too.
        e.input(&s.id, b"raw-bytes\n").unwrap();
        e.resize(&s.id, 90, 25).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut snap = String::new();
        while Instant::now() < deadline && !snap.contains("raw-bytes") {
            if let ServerFrame::Screen { text, .. } = e.snapshot(&s.id).unwrap() {
                snap = text;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(snap.contains("raw-bytes"), "snapshot: {snap}");

        e.supervise_tick().unwrap();
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Idle, "a running shell stays idle");

        e.detach(&s.id);
        let killed = e.kill(&s.id, false).unwrap();
        assert_eq!(killed.state, SessionState::Done);
        assert!(!tmux::session_alive(&s.id));
        e.remove(&s.id).unwrap();

        // Death detection: a session whose command exits non-zero is dead
        // (shell kind ⇒ no resume).
        let s2 = e
            .new_session(NewRequest {
                kind: SessionKind::Shell,
                worktree: Some(tmp.path().to_string_lossy().into()),
                argv: Some(vec!["sh".into(), "-c".into(), "exit 3".into()]),
                ..Default::default()
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = SessionState::Idle;
        while Instant::now() < deadline && !state.is_terminal() {
            e.supervise_tick().unwrap();
            state = e.get(&s2.id).unwrap().unwrap().state;
            std::thread::sleep(Duration::from_millis(100));
        }
        let s2 = e.get(&s2.id).unwrap().unwrap();
        assert_eq!(s2.state, SessionState::Dead);
        assert_eq!(s2.exit_code, Some(3), "PTY-reported exit code");
        tmux::kill_server();
        std::env::remove_var("SMOOTH_FLOW_TMUX_SOCKET");
    }
}
