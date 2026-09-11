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
use smooth_tmux::detect::PaneState;
use tokio::sync::{broadcast, oneshot};

use crate::harness::{FlowEventName, HarnessInfo, Manifest, Prefs, PromptAs, Registry, ResumeMode, ScrapeRules, SessionIdMode, StateSource, Vars};
use crate::protocol::{
    approval_keystroke, hook_event_text, map_hook_event, permission_detail, permission_reply, CandidateSpec, CloseOutcome, DaemonInfo, Decision, EventKind,
    FlowEvent, HookEvent, HookOutcome, ServerFrame,
};
use crate::pty::{OnOutput, PtyAttach};
use crate::store::{Attention, FanOut, FlowStore, NewSession, Pairing, Session, SessionKind, SessionState};
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
/// `prompt_as = "paste"`: how long after launch the prompt is pasted into the
/// harness's composer. ponytail: a fixed delay; scrape-for-idle first if a
/// harness turns out to boot slower than this.
const PASTE_DELAY: Duration = Duration::from_secs(4);
/// The `config` key harness prefs are stored under.
const HARNESS_PREFS_KEY: &str = "harness_prefs";

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
    /// `$HOME` — where `~/.smooth/harnesses/` and the `th pkg` index live
    /// (tests point this at a tempdir).
    pub home: PathBuf,
    /// `http://host:port` of the daemon hosting this engine, for the
    /// `{daemon_url}` placeholder (a `th code` pane connects back to it).
    pub daemon_url: Option<String>,
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
            home: dirs_next::home_dir().unwrap_or_default(),
            daemon_url: None,
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
    /// The tmux socket to create the session on (default: [`tmux::socket_name`]).
    pub tmux_socket: Option<String>,
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
    resume_attempts: HashMap<String, u32>,
    /// Session id → when its pending relaunch may fire.
    relaunch_at: HashMap<String, Instant>,
    /// Agent session id → (pid, claimed at): rule 4's duplicate-resume guard.
    claims: HashMap<String, (u32, Instant)>,
    /// Session id → when it was last resumed from a usage limit.
    limit_resumed_at: HashMap<String, Instant>,
    /// Session id → (prompt, when to paste it) for `prompt_as = "paste"`.
    paste_at: HashMap<String, (String, Instant)>,
    /// Compiled `[state.scrape]` rules per kind.
    rules: HashMap<String, Arc<ScrapeRules>>,
    /// Harness session ids adoption already refused (th-c103c1) — a stranger
    /// posts a hook on every tool call, and re-running git + `th pearls` for
    /// each one would be a shell-out storm.
    adopt_refused: HashMap<String, AdoptRefusal>,
    /// Watermark for re-broadcasting rows another daemon changed in the
    /// shared `flow.db` (th-c103c1).
    seen_changes_at: Option<DateTime<Utc>>,
}

struct Inner {
    store: Mutex<FlowStore>,
    tx: broadcast::Sender<ServerFrame>,
    ptys: Mutex<HashMap<String, Arc<PtyAttach>>>,
    pending: Mutex<HashMap<String, PendingApproval>>,
    rt: Mutex<Runtime>,
    info: DaemonInfo,
    default_project: PathBuf,
    home: PathBuf,
    daemon_url: Option<String>,
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

/// The tmux socket a session lives on. Rows from before th-d33afa carry no
/// socket and fall back to the daemon's default.
fn socket_of(s: &Session) -> String {
    s.tmux_socket.clone().unwrap_or_else(tmux::socket_name)
}

/// Whether this daemon owns (supervises) `s` — th-4f7866. A row is owned by
/// the daemon that created it, identified by the tmux socket name that daemon
/// was configured with; rows from before the `owner` column are owned by the
/// daemon whose socket matches the row's. Two daemons sharing one flow.db
/// (the default `th up` daemon + the SmoothFlow app's child, or an orphaned
/// instance) otherwise each declare the other's live panes "process vanished"
/// and race to relaunch them.
fn owned_here(s: &Session) -> bool {
    s.owner.clone().unwrap_or_else(|| socket_of(s)) == tmux::socket_name()
}

/// `(socket, tmux session)` of a launched session.
fn pane(s: &Session) -> Result<(String, String)> {
    if s.adopted {
        bail!(
            "session {} was adopted from a plain terminal (th-c103c1) — SmoothFlow never spawned its PTY, so there is no pane to attach; drive it where it is running",
            s.id
        );
    }
    let t = s.tmux_session.clone().ok_or_else(|| anyhow!("session {} has no tmux session", s.id))?;
    Ok((socket_of(s), t))
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

/// The login shell argv for `kind = shell`.
fn shell_argv() -> Vec<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    vec![shell, "-l".into()]
}

/// Build the argv for a fresh session: the login shell, or the manifest's
/// resolved binary + rendered `launch.argv` (th-0f6126; the per-kind launch
/// table of th-5c5457 is now the built-in manifests).
///
/// # Errors
/// When `kind` has no manifest.
pub fn default_argv(registry: &Registry, kind: &SessionKind, vars: &Vars<'_>) -> Result<Vec<String>> {
    if !kind.is_agent() {
        return Ok(shell_argv());
    }
    let m = manifest_for(registry, kind)?;
    let mut v = vec![m.resolve_binary()];
    v.extend(crate::harness::render_argv(&m.launch.argv, vars));
    Ok(v)
}

/// The manifest for an agent kind, or the error a caller should surface.
///
/// # Errors
/// When no manifest carries that name.
pub fn manifest_for<'r>(registry: &'r Registry, kind: &SessionKind) -> Result<&'r Manifest> {
    registry
        .get(kind.as_str())
        .ok_or_else(|| anyhow!("unknown harness kind `{kind}` — `th harness list` shows what this machine knows"))
}

/// The argv that resumes a dead agent session — the manifest's `[resume]`.
///
/// `resume_session` with a known harness session id renders `resume.argv`
/// behind the row's `argv[0]`; otherwise (no id yet, or `relaunch_command`,
/// or no manifest) the original argv is relaunched: a fresh session, not a
/// continuation.
#[must_use]
pub fn resume_argv(session: &Session, registry: &Registry) -> Vec<String> {
    let Some(m) = registry.get(session.kind.as_str()) else {
        return session.argv.clone();
    };
    match (m.resume.mode, session.agent_session_id.as_deref()) {
        (ResumeMode::ResumeSession, Some(id)) => {
            let bin = session.argv.first().cloned().unwrap_or_else(|| m.resolve_binary());
            let vars = Vars {
                session_id: Some(id),
                cwd: Some(&session.worktree),
                ..Default::default()
            };
            let mut v = vec![bin];
            v.extend(crate::harness::render_argv(&m.resume.argv, &vars));
            v
        }
        _ => session.argv.clone(),
    }
}

/// Rule 2's backoff: `base · 2^attempt`.
#[must_use]
pub fn resume_backoff(attempt: u32) -> Duration {
    RESUME_BACKOFF_BASE.saturating_mul(2u32.saturating_pow(attempt.min(10)))
}

/// Default title for a new session: the prompt, else what the worktree says
/// it is working on (th-c103c1 — the pearl's title, else the branch), else
/// the pearl id, else `kind · dir`.
#[must_use]
pub fn default_title(kind: &SessionKind, prompt: Option<&str>, pearl_id: Option<&str>, inferred: Option<&str>, worktree: &Path) -> String {
    if let Some(p) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        let short: String = p.chars().take(60).collect();
        return short;
    }
    if let Some(t) = inferred.map(str::trim).filter(|t| !t.is_empty()) {
        return t.to_string();
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

// ── adoption (th-c103c1) ──────────────────────────────────────────────────

/// The `config` key the adoption opt-in is stored under.
pub const ADOPT_KEY: &str = "adopt_plain_sessions";
/// `$SMOOTH_FLOW_ADOPT` overrides the stored opt-in (tests, and a one-off
/// daemon run).
pub const ADOPT_ENV: &str = "SMOOTH_FLOW_ADOPT";
/// An adopted session that has gone this long without a hook is presumed
/// gone: its terminal was closed without a `SessionEnd`, and nothing here can
/// see its process.
pub const ADOPTED_STALE_AFTER_SECS: i64 = 6 * 60 * 60;

/// Why adoption declined a hook.
///
/// Every refusal is a deliberate guard, and the reason is cached per harness
/// session so the check runs once, not per event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptRefusal {
    /// The opt-in is off (the default).
    Disabled,
    /// The hook carried no harness session id — nothing to key a row on.
    NoSessionId,
    /// The hook carried no cwd, or it is not a git repo. SmoothFlow tracks
    /// work on branches; a shell in `/tmp` is not fleet work.
    NotGit,
    /// `harness` names no manifest this machine knows.
    UnknownHarness,
    /// The cwd's project has never had a session here. Adopting it would pull
    /// unrelated repos into the fleet.
    UnknownProject,
    /// A permission request is the one event that must not be the first: the
    /// engine would hold the harness open for a decision about a session
    /// nobody is watching yet.
    PermissionFirst,
}

impl AdoptRefusal {
    /// A short reason for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "adoption is off",
            Self::NoSessionId => "no harness session id",
            Self::NotGit => "cwd is not a git worktree",
            Self::UnknownHarness => "unknown harness",
            Self::UnknownProject => "project has no sessions here",
            Self::PermissionFirst => "first event is a permission request",
        }
    }
}

/// The opt-in, pure: `$SMOOTH_FLOW_ADOPT` (`1`/`true`/`on` — or `0`/`false`/
/// `off`) beats the stored value, which defaults to OFF.
#[must_use]
pub fn adopt_setting(env: Option<&str>, stored: Option<&str>) -> bool {
    let truthy = |v: &str| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes");
    match env.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => truthy(v),
        None => stored.is_some_and(truthy),
    }
}

/// The manifest kind a hook's `harness` field names: the manifest name
/// itself, else the `-code` spelling the Claude Code hooks send
/// (`claude-code` → `claude`).
#[must_use]
pub fn kind_for_harness(registry: &Registry, harness: &str) -> Option<SessionKind> {
    let h = harness.trim().to_ascii_lowercase();
    if h.is_empty() {
        return None;
    }
    let name = if registry.get(&h).is_some() {
        h
    } else {
        let stripped = h.strip_suffix("-code")?.to_string();
        registry.get(&stripped).is_some().then_some(stripped)?
    };
    name.parse().ok()
}

/// Whether `project` is one the fleet already works in.
///
/// That means the daemon's own workspace, or a project some session row
/// already lives in. Every argument must already be [`canon`]icalised —
/// `/var/…` and `/private/var/…` are the same directory, and git always
/// answers with the resolved one.
#[must_use]
pub fn project_is_known(project: &str, default_project: &str, known: &[String]) -> bool {
    project == default_project || known.iter().any(|k| k == project)
}

/// A path with symlinks resolved, falling back to the input when it cannot be
/// resolved (a project that has since been deleted still compares by name).
#[must_use]
pub fn canon(p: &str) -> String {
    std::fs::canonicalize(p).map_or_else(|_| p.to_string(), |c| c.to_string_lossy().into_owned())
}

/// The adoption guards, pure over the facts. `Ok(kind)` means "create a row".
///
/// # Errors
/// The refusal reason, which the caller caches per harness session.
pub fn adoptable(
    ev_event: &str,
    session_id: &str,
    enabled: bool,
    inferred: Option<&crate::infer::Inferred>,
    kind: Option<SessionKind>,
    project_known: bool,
) -> std::result::Result<SessionKind, AdoptRefusal> {
    if !enabled {
        return Err(AdoptRefusal::Disabled);
    }
    if session_id.trim().is_empty() {
        return Err(AdoptRefusal::NoSessionId);
    }
    if ev_event == "PermissionRequest" {
        return Err(AdoptRefusal::PermissionFirst);
    }
    let kind = kind.ok_or(AdoptRefusal::UnknownHarness)?;
    if !inferred.is_some_and(|i| i.is_git) {
        return Err(AdoptRefusal::NotGit);
    }
    if !project_known {
        return Err(AdoptRefusal::UnknownProject);
    }
    Ok(kind)
}

/// Whether an adopted row has been silent long enough to call dead.
#[must_use]
pub fn adopted_is_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(updated_at).num_seconds() >= ADOPTED_STALE_AFTER_SECS
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
                home: cfg.home,
                daemon_url: cfg.daemon_url,
            }),
        })
    }

    // ── harnesses (th-0f6126) ─────────────────────────────────────────────

    /// Every manifest this engine knows: built-ins, `~/.smooth/harnesses/`,
    /// the project's `.smooth/harnesses/`, `th pkg` packages. Loaded fresh
    /// each call so `th harness add` needs no daemon restart.
    // ponytail: a few small files per call; cache when a profile says so.
    #[must_use]
    pub fn registry(&self) -> Registry {
        Registry::load(&self.inner.home, Some(&self.inner.default_project))
    }

    /// The stored sort/hide prefs.
    ///
    /// # Errors
    /// On a store failure.
    pub fn harness_prefs(&self) -> Result<Prefs> {
        Ok(self
            .with_store(|st| st.get_config(HARNESS_PREFS_KEY))?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    // ── pairings (th-d98fde) ────────────────────────────────────────────────

    /// Every paired phone.
    ///
    /// # Errors
    /// On a store failure.
    pub fn pairings(&self) -> Result<Vec<Pairing>> {
        self.with_store(FlowStore::list_pairings)
    }

    /// One pairing by relay device id.
    ///
    /// # Errors
    /// On a store failure.
    pub fn pairing(&self, device: &str) -> Result<Option<Pairing>> {
        self.with_store(|st| st.pairing(device))
    }

    /// Persist a new (or rotated) pairing.
    ///
    /// # Errors
    /// On a store failure.
    pub fn upsert_pairing(&self, p: &Pairing) -> Result<()> {
        self.with_store(|st| st.upsert_pairing(p))
    }

    /// Revoke a pairing; `true` when it existed.
    ///
    /// # Errors
    /// On a store failure.
    pub fn remove_pairing(&self, device: &str) -> Result<bool> {
        self.with_store(|st| st.remove_pairing(device))
    }

    /// Record that the phone was heard from.
    ///
    /// # Errors
    /// On a store failure.
    pub fn touch_pairing(&self, device: &str, at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        self.with_store(|st| st.touch_pairing(device, at))
    }

    /// The harness rows — `all = false` is the `flow.hello` list (hidden
    /// ones dropped), `all = true` is `GET /api/flow/harnesses`.
    ///
    /// # Errors
    /// On a store failure.
    pub fn harnesses(&self, all: bool) -> Result<Vec<HarnessInfo>> {
        let prefs = self.harness_prefs()?;
        Ok(self
            .registry()
            .infos(&prefs, all, &self.inner.home, &std::env::var_os("PATH").unwrap_or_default()))
    }

    /// `PUT /api/flow/harnesses/prefs`: replace `order` and/or `hidden`
    /// (a `None` leaves that half alone), persist, broadcast
    /// `flow.harnesses`, return the full list.
    ///
    /// # Errors
    /// When a name is not a known harness, or on a store failure.
    pub fn set_harness_prefs(&self, order: Option<Vec<String>>, hidden: Option<Vec<String>>) -> Result<Vec<HarnessInfo>> {
        let registry = self.registry();
        let mut prefs = self.harness_prefs()?;
        for (field, names) in [("order", &order), ("hidden", &hidden)] {
            if let Some(names) = names {
                if let Some(bad) = names.iter().find(|n| registry.get(n).is_none()) {
                    bail!("{field}: `{bad}` is not a known harness (th harness list --all)");
                }
            }
        }
        if let Some(o) = order {
            prefs.order = o;
        }
        if let Some(h) = hidden {
            prefs.hidden = h;
        }
        let raw = serde_json::to_string(&prefs)?;
        self.with_store(|st| st.set_config(HARNESS_PREFS_KEY, &raw))?;
        self.emit(ServerFrame::Harnesses {
            harnesses: self.harnesses(false)?,
        });
        self.harnesses(true)
    }

    /// Compiled scrape rules for a kind (cached per engine).
    fn rules_for(&self, registry: &Registry, kind: &SessionKind) -> Option<Arc<ScrapeRules>> {
        if let Some(r) = self.rt().rules.get(kind.as_str()) {
            return Some(r.clone());
        }
        let m = registry.get(kind.as_str())?;
        let rules = Arc::new(ScrapeRules::compile(&m.state.scrape).ok()?);
        self.rt().rules.insert(kind.as_str().to_string(), rules.clone());
        Some(rules)
    }

    /// The pane environment a manifest asks for, rendered for `s`.
    fn launch_env(&self, m: Option<&Manifest>, s: &Session) -> Vec<(String, String)> {
        m.map(|m| {
            crate::harness::render_env(
                &m.launch.env,
                &Vars {
                    session_id: s.agent_session_id.as_deref(),
                    cwd: Some(&s.worktree),
                    daemon_url: self.inner.daemon_url.as_deref(),
                    ..Default::default()
                },
            )
        })
        .unwrap_or_default()
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
            harnesses: self.harnesses(false)?,
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
            if before.as_ref().map(|b| b.state) != Some(s.state) {
                self.event(id, EventKind::System, &state_line(s));
            }
        }
        Ok(after)
    }

    // ── events (th-d33afa) ───────────────────────────────────────────────

    /// Append one line to the session's event stream and broadcast it as
    /// `flow.event`. Never fails the caller: the stream is a view, the
    /// state change it describes already happened.
    fn event(&self, id: &str, kind: EventKind, text: &str) {
        match self.with_store(|st| st.add_event(id, kind, text)) {
            Ok(event) => self.emit(ServerFrame::Event { id: id.to_string(), event }),
            Err(e) => tracing::warn!(session = %id, error = %e, "flow: recording event"),
        }
    }

    /// The buffered event stream (last [`crate::store::EVENT_BUFFER`]),
    /// oldest first — replayed to a client on `flow.attach`.
    ///
    /// # Errors
    /// On a store failure.
    pub fn events(&self, id: &str) -> Result<Vec<FlowEvent>> {
        self.with_store(|st| st.events(id))
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
        let registry = self.registry();
        let manifest = if req.kind.is_agent() {
            Some(manifest_for(&registry, &req.kind)?)
        } else {
            None
        };
        let agent_session_id = manifest
            .filter(|m| m.launch.session_id == SessionIdMode::Preassigned)
            .map(|_| uuid::Uuid::new_v4().to_string());
        let prompt = req.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty());
        let paste = manifest.is_some_and(|m| m.launch.prompt_as == PromptAs::Paste);
        let worktree_s = worktree.to_string_lossy().into_owned();
        let vars = Vars {
            prompt: if paste { None } else { prompt },
            session_id: agent_session_id.as_deref(),
            cwd: Some(&worktree_s),
            model: req.model.as_deref(),
            daemon_url: self.inner.daemon_url.as_deref(),
        };
        let mut argv = match req.argv.clone().filter(|a| !a.is_empty()) {
            Some(a) => a,
            None => default_argv(&registry, &req.kind, &vars)?,
        };
        // An explicit bare `claude`/`codex`/`opencode` gets the same shim-safe
        // resolution as the default argv.
        if let Some(m) = manifest {
            if argv.first().is_some_and(|a| m.is_bare_name(a)) {
                argv[0] = m.resolve_binary();
            }
        }
        // th-c103c1: nothing here is demanded of the caller — the pearl, the
        // branch and the title are inferred from the worktree when they were
        // not given. An explicit value always wins.
        let inferred = crate::infer::gather(&worktree);
        let branch = inferred.branch.clone().or_else(|| git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).ok());
        let pearl_id = req.pearl_id.clone().or_else(|| inferred.pearl_id.clone());
        let title = req
            .title
            .clone()
            .unwrap_or_else(|| default_title(&req.kind, req.prompt.as_deref(), pearl_id.as_deref(), Some(&inferred.title), &worktree));
        let session = self.with_store(|st| {
            st.create(NewSession {
                kind: Some(req.kind.clone()),
                title,
                project: project.to_string_lossy().into_owned(),
                worktree: worktree.to_string_lossy().into_owned(),
                branch,
                pearl_id: pearl_id.clone(),
                agent_session_id: agent_session_id.clone(),
                argv: argv.clone(),
                tmux_session: None,
                tmux_socket: Some(req.tmux_socket.clone().unwrap_or_else(tmux::socket_name)),
                owner: Some(tmux::socket_name()),
                fan_out_id: req.fan_out_id.clone(),
                adopted: false,
            })
        })?;
        let env = self.launch_env(manifest, &session);
        let session = self.launch(&session, &argv, &env)?;
        // A shell has no hooks and nothing to scrape — it is simply ready.
        if !session.kind.is_agent() {
            return Ok(self.set_state(&session.id, SessionState::Idle, None)?.unwrap_or(session));
        }
        if let (true, Some(p)) = (paste, prompt) {
            self.rt().paste_at.insert(session.id.clone(), (p.to_string(), Instant::now() + PASTE_DELAY));
        }
        Ok(session)
    }

    /// Launch `argv` in the session's tmux session (named after the id),
    /// record pid + start time, broadcast.
    fn launch(&self, session: &Session, argv: &[String], env: &[(String, String)]) -> Result<Session> {
        let tmux_name = session.id.clone();
        let sock = socket_of(session);
        if tmux::session_alive(&sock, &tmux_name) {
            tmux::kill_session(&sock, &tmux_name);
        }
        let pid = tmux::launch_env(&sock, &tmux_name, Path::new(&session.worktree), argv, env)?;
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
        let (sock, tmux_name) = pane(&session)?;
        if !tmux::session_alive(&sock, &tmux_name) {
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
        let pty = PtyAttach::spawn(&tmux::attach_argv(&sock, &tmux_name), cols, rows, on_output)?;
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
        let (cols, rows) = pane(&self.require(id)?)
            .ok()
            .and_then(|(k, t)| tmux::pane_size(&k, &t).ok())
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
        let (k, t) = pane(&s)?;
        tmux::send_text(&k, &t, text)?;
        self.event(id, EventKind::User, text);
        Ok(())
    }

    /// A named tmux key (`Enter`, `Escape`, `1`) into the pane — answering a
    /// dialog, not steering: no bracketed paste, no `User` event row. Harness
    /// validation uses it to accept a first-run prompt's default (th-473294).
    ///
    /// # Errors
    /// When the session is unknown or tmux refuses.
    pub fn send_key(&self, id: &str, key: &str) -> Result<()> {
        let s = self.require(id)?;
        let (k, t) = pane(&s)?;
        tmux::send_key(&k, &t, key)
    }

    /// `flow.snapshot`: plain-text visible pane.
    ///
    /// # Errors
    /// When the session is unknown or tmux refuses.
    pub fn snapshot(&self, id: &str) -> Result<ServerFrame> {
        let s = self.require(id)?;
        let (k, t) = pane(&s)?;
        let (cols, rows) = tmux::pane_size(&k, &t)?;
        Ok(ServerFrame::Screen {
            id: id.to_string(),
            cols,
            rows,
            text: tmux::capture_visible(&k, &t)?,
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
            let (k, t) = pane(&s)?;
            tmux::send_key(&k, &t, approval_keystroke(decision))?;
        }
        self.event(id, EventKind::User, &format!("approve: {}", decision.as_str()));
        self.set_state(id, SessionState::Working, None)?;
        Ok(())
    }

    /// `flow.kill`: kill the process tree; optionally relaunch with `--resume`.
    ///
    /// # Errors
    /// When the session is unknown or the relaunch fails.
    pub fn kill(&self, id: &str, resume: bool) -> Result<Session> {
        let s = self.require(id)?;
        if s.adopted {
            bail!(
                "session {id} was adopted from a plain terminal — SmoothFlow does not own its process and cannot {} it; stop it where it is running (`th flow close {id}` drops the row)",
                if resume { "resume" } else { "kill" }
            );
        }
        let sock = socket_of(&s);
        let tmux_name = s.tmux_session.clone();
        if let Some(pid) = s.pid {
            if proc::is_alive(pid, s.pid_start) {
                proc::kill_tree(pid, KILL_GRACE);
            }
        }
        let exit = tmux_name.as_deref().and_then(|t| tmux::pane_exit_status(&sock, t).ok().flatten());
        if let Some(t) = &tmux_name {
            tmux::kill_session(&sock, t);
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
            tmux::kill_session(&socket_of(&s), t);
        }
        self.with_store(|st| st.remove(id))?;
        self.emit(ServerFrame::SessionRemoved { id: id.to_string() });
        Ok(())
    }

    /// `flow.close` (th-e126cc): finish a session for good — close its pearl,
    /// remove its worktree and branch once the branch is merged, drop the row,
    /// broadcast `flow.session.removed`. A live session is killed first.
    ///
    /// Everything is validated before anything changes: a dirty worktree, or
    /// a branch not merged into the project (by ancestry, or a merged PR per
    /// `gh` — the repos squash-merge), is refused with nothing touched unless
    /// `force`. The main checkout is never removed.
    ///
    /// # Errors
    /// When the session is unknown, the worktree is dirty or unmerged (and
    /// `!force`), or `th` / `git` refuse.
    pub fn close(&self, id: &str, close_pearl: bool, remove_worktree: bool, force: bool) -> Result<CloseOutcome> {
        let s = self.require(id)?;
        let project = Path::new(&s.project);
        let wt = Path::new(&s.worktree);
        let removable = remove_worktree && wt != project && wt.is_dir();
        let branch = if removable { worktree_branch(wt, s.branch.as_deref()) } else { None };
        if removable && !force {
            let dirty = git(wt, &["status", "--porcelain"]).unwrap_or_default();
            if !dirty.trim().is_empty() {
                bail!("worktree {} has uncommitted changes — commit or stash them, or close with force", wt.display());
            }
            if let Some(b) = &branch {
                if !branch_merged(project, wt, b) {
                    bail!("branch {b} is not merged into {} — merge the PR first, or close with force", project.display());
                }
            }
        }
        if !s.state.is_terminal() {
            if s.adopted {
                // Nothing here can stop it, and removing a live session's
                // worktree out from under it would be destructive.
                if !force {
                    bail!(
                        "session {id} is adopted and still {} — stop it where it is running, or close with force",
                        s.state
                    );
                }
            } else {
                self.kill(id, false)?;
            }
        }
        let mut out = CloseOutcome {
            id: id.to_string(),
            ..Default::default()
        };
        if close_pearl {
            if let Some(p) = &s.pearl_id {
                th(project, &["pearls", "close", p])?;
                out.pearl_closed = Some(p.clone());
            }
        }
        if removable {
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&s.worktree);
            git(project, &args)?;
            out.worktree_removed = Some(s.worktree.clone());
            if let Some(b) = &branch {
                // Merged was established above (ancestry or a merged PR — a
                // squash merge is not an ancestor, so `-d` would refuse it).
                if git(project, &["branch", "-D", b]).is_ok() {
                    out.branch_deleted = Some(b.clone());
                }
            }
        }
        self.with_store(|st| st.remove(id))?;
        self.emit(ServerFrame::SessionRemoved { id: id.to_string() });
        Ok(out)
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
        let registry = self.registry();
        let argv = resume_argv(s, &registry);
        let env = self.launch_env(registry.get(s.kind.as_str()), s);
        // A relaunched process hasn't reported a hook yet — let the scraper
        // drive state until it does (a resumed opencode session emits no
        // session.created; measured 2026-09-08).
        self.with_store(|st| st.set_state_source(&s.id, "inferred"))?;
        let launched = self.launch(s, &argv, &env)?;
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
        let mut found = self.with_store(|st| st.get_by_agent_session(&ev.session_id))?;
        // opencode / codex can't pre-assign a session id: their first hook
        // from a worktree binds to the newest id-less agent row there.
        if found.is_none() && !ev.session_id.is_empty() {
            if let Some(cwd) = ev.cwd.as_deref().filter(|c| !c.is_empty()) {
                found = self.bind_by_cwd(cwd, &ev.session_id)?;
            }
        }
        // th-c103c1: still nobody? This may be a `claude`/`codex` someone
        // started in a plain terminal — adopt it into the fleet.
        if found.is_none() {
            found = self.try_adopt(&ev)?;
        }
        let Some(s) = found else {
            tracing::debug!(session = %ev.session_id, event = %ev.event, "flow hook for an unknown session");
            return Ok(HookReply::Immediate(json!({})));
        };
        // th-0f6126: the manifest says how this harness's events read
        // (`state.hooks.event_map`), and whether they count as `hooks` or
        // `native` state; an empty map is the Claude Code table.
        let manifest = self.registry().get(s.kind.as_str()).cloned();
        let source = manifest
            .as_ref()
            .filter(|m| m.state.source == StateSource::Native)
            .map_or("hooks", |_| StateSource::Native.as_str());
        if s.state_source != source {
            self.with_store(|st| st.set_state_source(&s.id, source))?;
            if let Some(s) = self.get(&s.id)? {
                self.emit_session(&s);
            }
        }
        if let Some((kind, text)) = hook_event_text(&ev.event, &ev.payload) {
            self.event(&s.id, kind, &text);
        }
        let outcome = manifest
            .as_ref()
            .filter(|m| !m.state.hooks.event_map.is_empty())
            .map_or_else(|| map_hook_event(&ev.event, &ev.payload), |m| mapped_outcome(m, &ev));
        match outcome {
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
            HookOutcome::Ended => {
                // Engine-spawned rows learn their exit from the PTY; an
                // adopted one has no pane, so `SessionEnd` IS the end.
                if s.adopted {
                    self.set_state(&s.id, SessionState::Done, None)?;
                }
            }
            HookOutcome::None => {}
        }
        Ok(HookReply::Immediate(json!({})))
    }

    /// Whether hooks from sessions this engine never launched are adopted
    /// (th-c103c1). Off unless `$SMOOTH_FLOW_ADOPT` or the stored opt-in says
    /// otherwise — adoption puts rows in the fleet that the user did not ask
    /// for, so it is theirs to turn on.
    #[must_use]
    pub fn adopt_enabled(&self) -> bool {
        let stored = self.with_store(|st| st.get_config(ADOPT_KEY)).ok().flatten();
        adopt_setting(std::env::var(ADOPT_ENV).ok().as_deref(), stored.as_deref())
    }

    /// Turn adoption on or off (persisted in the flow store).
    ///
    /// # Errors
    /// On a store failure.
    pub fn set_adopt(&self, on: bool) -> Result<()> {
        self.with_store(|st| st.set_config(ADOPT_KEY, if on { "1" } else { "0" }))?;
        self.rt().adopt_refused.clear();
        Ok(())
    }

    /// What a session started in `cwd` would be working on — the New Session
    /// dialog's read-only context (`GET /api/flow/infer`). `None` uses the
    /// daemon's workspace.
    #[must_use]
    pub fn infer_context(&self, cwd: Option<&Path>) -> crate::infer::Inferred {
        crate::infer::gather(cwd.unwrap_or(&self.inner.default_project))
    }

    /// Adopt the harness session `ev` belongs to, if every guard allows it.
    /// A refusal is cached per harness session id so the git/`th` shell-outs
    /// happen once, not on every hook event.
    fn try_adopt(&self, ev: &HookEvent) -> Result<Option<Session>> {
        // Bound before the `if let` so the runtime lock is not held across
        // the body.
        let cached = self.rt().adopt_refused.get(&ev.session_id).copied();
        if let Some(why) = cached {
            tracing::trace!(session = %ev.session_id, why = why.as_str(), "flow: adoption already refused");
            return Ok(None);
        }
        let enabled = self.adopt_enabled();
        let kind = kind_for_harness(&self.registry(), &ev.harness);
        // Cheap guards first: inference shells out to git and `th`.
        if let Err(why) = adoptable(&ev.event, &ev.session_id, enabled, None, kind.clone(), true) {
            if why != AdoptRefusal::NotGit {
                return Ok(self.refuse_adoption(&ev.session_id, why));
            }
        }
        let Some(cwd) = ev.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) else {
            return Ok(self.refuse_adoption(&ev.session_id, AdoptRefusal::NotGit));
        };
        let inferred = crate::infer::gather(Path::new(cwd));
        let known: Vec<String> = self.with_store(FlowStore::projects)?.iter().map(|k| canon(k)).collect();
        let project_known = project_is_known(&canon(&inferred.project), &canon(&self.inner.default_project.to_string_lossy()), &known);
        let kind = match adoptable(&ev.event, &ev.session_id, enabled, Some(&inferred), kind, project_known) {
            Ok(k) => k,
            Err(why) => return Ok(self.refuse_adoption(&ev.session_id, why)),
        };
        let agent_id = ev.session_id.clone();
        // Check-and-create under the store lock: two hooks from the same
        // session can land concurrently (the script is fire-and-forget).
        let created = self.with_store(|st| {
            if let Some(existing) = st.get_by_agent_session(&agent_id)? {
                return Ok::<_, anyhow::Error>(Some((existing, false)));
            }
            let s = st.create(NewSession {
                kind: Some(kind.clone()),
                title: inferred.title.clone(),
                project: inferred.project.clone(),
                worktree: inferred.worktree.clone(),
                branch: inferred.branch.clone(),
                pearl_id: inferred.pearl_id.clone(),
                agent_session_id: Some(agent_id.clone()),
                argv: Vec::new(),
                tmux_session: None,
                tmux_socket: None,
                owner: Some(tmux::socket_name()),
                fan_out_id: None,
                adopted: true,
            })?;
            Ok(Some((s, true)))
        })?;
        let Some((session, fresh)) = created else { return Ok(None) };
        if fresh {
            tracing::info!(
                session = %session.id, harness_session = %agent_id, kind = %kind, worktree = %session.worktree,
                pearl = session.pearl_id.as_deref().unwrap_or("-"),
                "flow: adopted a harness session started outside SmoothFlow"
            );
            self.event(&session.id, EventKind::System, "adopted — started outside SmoothFlow (no pane to attach)");
            self.emit_session(&session);
        }
        Ok(Some(session))
    }

    /// Cache a refusal and return `None`.
    fn refuse_adoption(&self, session_id: &str, why: AdoptRefusal) -> Option<Session> {
        tracing::debug!(session = %session_id, why = why.as_str(), "flow: not adopting");
        self.rt().adopt_refused.insert(session_id.to_string(), why);
        None
    }

    /// Bind harness session `agent_session_id` to the newest id-less agent
    /// row in `cwd`, so later hooks (and `--session`/`resume`) find it.
    fn bind_by_cwd(&self, cwd: &str, agent_session_id: &str) -> Result<Option<Session>> {
        let Some(row) = self.with_store(|st| st.find_bindable(cwd))? else {
            return Ok(None);
        };
        self.with_store(|st| st.set_agent_session(&row.id, agent_session_id))?;
        if let Some(pid) = row.pid {
            self.rt().claims.insert(agent_session_id.to_string(), (pid, Instant::now()));
        }
        tracing::info!(session = %row.id, harness_session = %agent_session_id, "flow: bound harness session id from its first hook");
        self.get(&row.id)
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

    /// One supervision pass over every live session **this daemon owns**
    /// (th-4f7866 — see [`owned_here`]). Cheap when nothing changed; safe to
    /// call every couple of seconds.
    ///
    /// # Errors
    /// On a store failure (per-session tmux/ps errors are logged, not raised).
    pub fn supervise_tick(&self) -> Result<()> {
        let now = Utc::now();
        for s in self.with_store(FlowStore::list_live)?.into_iter().filter(owned_here) {
            if let Err(e) = self.supervise_one(&s, now) {
                tracing::warn!(session = %s.id, error = %e, "flow supervision");
            }
        }
        self.rebroadcast_external_changes(now)?;
        Ok(())
    }

    /// Re-broadcast rows another daemon changed in the shared `flow.db`
    /// (th-c103c1). Big Smooth and the SmoothFlow app's child daemon share
    /// one file but not one broadcast channel, so a hook that lands on the
    /// other one is invisible to this one's clients until something re-reads
    /// the store. A `Session` frame is an upsert, so re-emitting a row this
    /// engine already emitted is harmless.
    fn rebroadcast_external_changes(&self, now: DateTime<Utc>) -> Result<()> {
        let since = self.rt().seen_changes_at;
        let Some(since) = since else {
            // First tick: take the watermark, emit nothing (clients just got
            // the whole list in `flow.hello`).
            self.rt().seen_changes_at = Some(now);
            return Ok(());
        };
        let changed = self.with_store(|st| st.changed_since(since))?;
        if let Some(latest) = changed.iter().map(|s| s.updated_at).max() {
            self.rt().seen_changes_at = Some(latest);
        }
        for s in changed.iter().filter(|s| !owned_here(s)) {
            self.emit_session(s);
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
        // An adopted session has no pane and no pid here: the only liveness
        // signal is its own hooks, so silence is the only thing to act on.
        if s.adopted {
            if adopted_is_stale(s.updated_at, now) {
                let att = Attention::new("crashed").with_detail("adopted session went silent — its terminal is gone");
                self.set_state(&s.id, SessionState::Dead, Some(att))?;
            }
            return Ok(());
        }
        let Some(t) = s.tmux_session.as_deref() else { return Ok(()) };
        let sock = socket_of(s);
        if !tmux::session_alive(&sock, t) {
            return self.on_death(s, None);
        }
        if let Some(code) = tmux::pane_exit_status(&sock, t)? {
            self.with_store(|st| st.set_exit_code(&s.id, Some(code)))?;
            tmux::kill_session(&sock, t);
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
        // `prompt_as = "paste"`: the composer should be up by now.
        let paste = self.rt().paste_at.get(&s.id).filter(|(_, at)| Instant::now() >= *at).map(|(p, _)| p.clone());
        if let Some(p) = paste {
            self.rt().paste_at.remove(&s.id);
            self.send(&s.id, &p)?;
        }
        // Usage-limit resume due?
        if s.state == SessionState::Limited {
            let due = s.attention.as_ref().and_then(|a| a.resume_at).is_some_and(|at| now >= at);
            if due {
                tracing::info!(session = %s.id, "flow: usage limit window passed — resuming");
                self.rt().limit_resumed_at.insert(s.id.clone(), Instant::now());
                tmux::send_key(&sock, t, "Enter")?;
                self.set_state(&s.id, SessionState::Working, None)?;
            }
            return Ok(());
        }
        // Scrape the visible pane: limits always; approvals when hooks
        // didn't report one; working/idle only when hooks never spoke.
        let pane = tmux::capture_visible(&sock, t)?;
        let hooks_seen = s.state_source != "inferred";
        let Some(rules) = self.rules_for(&self.registry(), &s.kind) else {
            return Ok(());
        };
        let scrape = rules.detect(&pane);
        match scrape.state {
            PaneState::UsageLimit => {
                let recently_resumed = self.rt().limit_resumed_at.get(&s.id).is_some_and(|at| at.elapsed() < LIMIT_REARM_GRACE);
                if !recently_resumed {
                    let at = limit::resume_at(scrape.reset_text.as_deref().unwrap_or(&pane), now);
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
                kind: c.kind.clone(),
                worktree: Some(worktree.to_string_lossy().into_owned()),
                project: Some(project.to_string_lossy().into_owned()),
                pearl_id: child.clone().or_else(|| Some(pearl_id.to_string())),
                prompt: Some(prompt.to_string()),
                argv: None,
                title: Some(format!("{pearl_id} · {}", c.label)),
                model: c.model.clone(),
                fan_out_id: Some(fo.id.clone()),
                tmux_socket: None,
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
        let dirty: Vec<String> = git(wt, &["status", "--porcelain"]).unwrap_or_default().lines().filter_map(dirty_path).collect();
        // `th pearls show <id> --handoff --json` (lane C, th-9483e8) is the
        // packet {pearl, handoff, checkpoints, blocks, pr}; an older `th`
        // degrades to the human text as `pearl.text`.
        let project = Path::new(&s.project);
        let packet = s
            .pearl_id
            .as_deref()
            .and_then(|p| th(project, &["pearls", "show", p, "--handoff", "--json"]).ok())
            .and_then(|out| serde_json::from_str::<Value>(&out).ok())
            .filter(Value::is_object);
        let pearl_json = packet
            .as_ref()
            .and_then(|p| p.get("pearl").cloned())
            .or_else(|| {
                s.pearl_id
                    .as_deref()
                    .and_then(|p| th(project, &["pearls", "show", p]).ok().map(|text| json!({ "id": p, "text": text })))
            })
            .unwrap_or(Value::Null);
        let from_packet = |key: &str| packet.as_ref().and_then(|p| p.get(key).cloned()).filter(|v| !v.is_null());
        let pr = s.branch.as_deref().and_then(|b| pr_for_branch(wt, b)).or_else(|| from_packet("pr"));
        Ok(json!({
            "pearl": pearl_json,
            "handoff": {
                "worktree": s.worktree,
                "branch": s.branch,
                "head": head,
                "dirty": dirty,
                "agent_session_id": s.agent_session_id,
                "next": packet.as_ref().and_then(|p| p.pointer("/handoff/next").cloned()).unwrap_or(Value::Null),
            },
            "checkpoints": from_packet("checkpoints").unwrap_or_else(|| json!([])),
            "blocks": from_packet("blocks").unwrap_or_else(|| json!([])),
            "pr": pr.unwrap_or(Value::Null),
        }))
    }
}

/// A hook event through a manifest's `state.hooks.event_map`.
fn mapped_outcome(m: &Manifest, ev: &HookEvent) -> HookOutcome {
    match m.map_event(&ev.event) {
        Some(FlowEventName::Working) => HookOutcome::Working,
        Some(FlowEventName::Idle) => HookOutcome::Idle,
        Some(FlowEventName::NeedsYou) => {
            let reason = ev.payload.get("reason").and_then(Value::as_str).unwrap_or("question");
            let detail = ev
                .payload
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| permission_detail(&ev.payload), str::to_string);
            HookOutcome::NeedsYou(Attention::new(reason).with_detail(detail))
        }
        Some(FlowEventName::Ended) => HookOutcome::Ended,
        Some(FlowEventName::Ignore) | None => HookOutcome::None,
    }
}

/// The path of one `git status --porcelain` line. Not a fixed offset: `git()`
/// trims stdout, so the first line loses its leading status space
/// (` M apps/x` → `M apps/x`) — th-f4073b's missing first character.
fn dirty_path(line: &str) -> Option<String> {
    let (_, path) = line.trim_start().split_once(' ')?;
    let path = path.trim_start();
    (!path.is_empty()).then(|| path.to_string())
}

/// The system event line for a state change: `needs_you · permission: Bash: ls`.
fn state_line(s: &Session) -> String {
    let mut line = s.state.to_string();
    if let Some(a) = &s.attention {
        line.push_str(" · ");
        line.push_str(&a.reason);
        if let Some(d) = a.detail.as_deref().filter(|d| !d.is_empty()) {
            line.push_str(": ");
            line.push_str(d);
        }
    }
    line
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

/// The branch checked out in `wt` — the recorded one, else `HEAD`'s name;
/// `None` when detached.
fn worktree_branch(wt: &Path, recorded: Option<&str>) -> Option<String> {
    recorded
        .map(str::to_string)
        .or_else(|| git(wt, &["rev-parse", "--abbrev-ref", "HEAD"]).ok())
        .filter(|b| !b.is_empty() && b != "HEAD")
}

/// Whether `branch` is merged into `project`'s HEAD: an ancestor of it, or
/// (squash merges leave no ancestry) the branch's PR is merged per `gh`.
fn branch_merged(project: &Path, wt: &Path, branch: &str) -> bool {
    let ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", branch, "HEAD"])
        .current_dir(project)
        .output()
        .is_ok_and(|o| o.status.success());
    ancestor || pr_merged(wt, branch)
}

/// Whether `gh` knows a merged PR for `branch` (false when gh is absent,
/// unauthenticated, or there is none).
fn pr_merged(cwd: &Path, branch: &str) -> bool {
    Command::new("gh")
        .args(["pr", "list", "--head", branch, "--state", "merged", "--limit", "1", "--json", "number"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false)
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
            home: tmp.join("home"),
            daemon_url: Some("http://127.0.0.1:1".into()),
        })
        .unwrap()
    }

    fn reg() -> Registry {
        Registry::builtin()
    }

    #[test]
    fn slug_and_title_helpers() {
        assert_eq!(slugify("Fix the Auth bug!!", 24), "fix-the-auth-bug");
        assert_eq!(slugify("   ", 24), "");
        assert!(slugify(&"x".repeat(100), 10).len() <= 10);
        assert_eq!(
            default_title(&SessionKind::Claude, Some("  do it  "), None, Some("inferred"), Path::new("/a/b")),
            "do it"
        );
        assert_eq!(
            default_title(&SessionKind::Claude, None, Some("th-1"), Some("Pearl title"), Path::new("/a/b")),
            "Pearl title"
        );
        assert_eq!(default_title(&SessionKind::Claude, None, Some("th-1"), None, Path::new("/a/b")), "th-1");
        assert_eq!(default_title(&SessionKind::Shell, None, None, None, Path::new("/a/b")), "shell · b");
        assert_eq!(
            default_title(&"th-code".parse().unwrap(), None, None, Some("  "), Path::new("/a/b")),
            "th-code · b"
        );
    }

    /// th-0f6126 regression: the three built-in manifests reproduce EXACTLY
    /// the launch table, restore verbs and binary resolution the engine had
    /// hard-coded (th-5c5457 / th-b423aa) — argv[0] is whatever this machine
    /// resolves, the tail is the table.
    #[test]
    fn builtin_manifests_reproduce_the_hardcoded_launch_table() {
        let r = reg();
        let argv = |kind: &str, id: Option<&str>, model: Option<&str>, prompt: Option<&str>| {
            default_argv(
                &r,
                &kind.parse().unwrap(),
                &Vars {
                    prompt,
                    session_id: id,
                    model,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let tail = |kind: &str, id: Option<&str>, model: Option<&str>, prompt: Option<&str>| argv(kind, id, model, prompt)[1..].to_vec();
        assert!(argv("claude", None, None, None)[0].ends_with("claude"));
        assert_eq!(
            tail("claude", Some("u"), Some("opus"), Some("hi")),
            vec!["--session-id", "u", "--model", "opus", "hi"]
        );
        assert!(tail("claude", None, None, Some("  ")).is_empty());
        assert_eq!(tail("codex", None, None, Some("p")), vec!["p"]);
        assert_eq!(tail("codex", None, Some("m"), Some("p")), vec!["--model", "m", "p"]);
        assert_eq!(tail("opencode", None, Some("m"), Some("p")), vec!["--model", "m", "--prompt", "p"]);
        assert!(argv("opencode", None, None, None)[0].ends_with("opencode"));
        assert_eq!(argv("shell", None, None, None)[1], "-l");
        // th code: the prompt is pasted, not passed; env carries the flow id + daemon.
        assert_eq!(tail("th-code", Some("fs-1"), Some("m"), Some("p")), vec!["code", "--model", "m"]);
        assert!(argv("th-code", None, None, None)[0].ends_with("th"));
        assert!(default_argv(&r, &"aider".parse().unwrap(), &Vars::default())
            .unwrap_err()
            .to_string()
            .contains("unknown harness kind"));

        // Restore per kind, argv[0] reused.
        let row = |kind: &str, id: Option<&str>, argv: &[&str]| Session {
            kind: kind.parse().unwrap(),
            agent_session_id: id.map(String::from),
            argv: argv.iter().map(|s| (*s).to_string()).collect(),
            ..blank()
        };
        assert_eq!(resume_argv(&row("claude", Some("u"), &["claude"]), &r), vec!["claude", "--resume", "u"]);
        assert_eq!(
            resume_argv(&row("opencode", Some("s"), &["/x/opencode", "--prompt", "hi"]), &r),
            vec!["/x/opencode", "--session", "s"]
        );
        assert_eq!(
            resume_argv(&row("codex", Some("t-9"), &["/x/codex", "hi"]), &r),
            vec!["/x/codex", "resume", "t-9"]
        );
        let no_id = row("codex", None, &["/x/codex", "hi"]);
        assert_eq!(resume_argv(&no_id, &r), no_id.argv, "no id yet ⇒ relaunch, not resume");
        let th = row("th-code", Some("fs-1"), &["/x/th", "code"]);
        assert_eq!(resume_argv(&th, &r), th.argv, "relaunch_command ⇒ the original argv");
        let unknown = row("aider", Some("x"), &["aider"]);
        assert_eq!(resume_argv(&unknown, &r), unknown.argv, "no manifest ⇒ relaunch");
    }

    /// A blank session row for pure-function tests.
    fn blank() -> Session {
        crate::store::FlowStore::open_in_memory()
            .unwrap()
            .create(NewSession {
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap()
    }

    /// th-5c5457: opencode/codex learn their harness session id from the
    /// first hook out of their worktree; from then on hooks and resume work.
    #[test]
    fn first_hook_from_a_worktree_binds_an_idless_agent_row() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let wt = tmp.path().to_string_lossy().into_owned();
        // A throwaway tmux server: the relaunch below really launches when tmux
        // is installed, and must never land on the user's flow socket.
        let sock = format!("flow-test-{}", std::process::id());
        let mk = |kind: SessionKind, argv: Vec<&str>| {
            e.with_store(|st| {
                st.create(NewSession {
                    kind: Some(kind),
                    argv: argv.into_iter().map(String::from).collect(),
                    project: wt.clone(),
                    worktree: wt.clone(),
                    tmux_socket: Some(sock.clone()),
                    ..Default::default()
                })
            })
            .unwrap()
        };
        let shell = mk(SessionKind::Shell, vec!["sh"]);
        let oc = mk(SessionKind::Opencode, vec!["/x/opencode", "--prompt", "hi"]);
        let ev = |event: &str, sid: &str, cwd: &str| HookEvent {
            harness: "opencode".into(),
            event: event.into(),
            session_id: sid.into(),
            cwd: Some(cwd.into()),
            payload: json!({}),
        };
        // Wrong cwd ⇒ nothing binds.
        e.hook(ev("SessionStart", "ses_1", "/elsewhere")).unwrap();
        assert_eq!(e.get(&oc.id).unwrap().unwrap().agent_session_id, None);
        // Right cwd ⇒ the newest id-less AGENT row binds (never the shell).
        e.hook(ev("SessionStart", "ses_1", &wt)).unwrap();
        let bound = e.get(&oc.id).unwrap().unwrap();
        assert_eq!(bound.agent_session_id.as_deref(), Some("ses_1"));
        assert_eq!(bound.state_source, "hooks");
        assert_eq!(e.get(&shell.id).unwrap().unwrap().agent_session_id, None);
        // A relaunch (fails without tmux, launches a dead `/x/opencode` pane
        // with it) resets the source first either way.
        let relaunched = e.relaunch(&bound);
        let _ = std::process::Command::new("tmux").args(["-L", &sock, "kill-server"]).output();
        drop(relaunched);
        assert_eq!(
            e.get(&oc.id).unwrap().unwrap().state_source,
            "inferred",
            "scraping covers the gap until hooks speak again"
        );
        // Later hooks find it by id; a second unknown id does not steal it.
        e.hook(ev("Stop", "ses_1", &wt)).unwrap();
        assert_eq!(e.get(&oc.id).unwrap().unwrap().state, SessionState::Idle);
        e.hook(ev("SessionStart", "ses_2", &wt)).unwrap();
        assert_eq!(e.get(&oc.id).unwrap().unwrap().agent_session_id.as_deref(), Some("ses_1"));
        // Restore mode per kind.
        assert_eq!(resume_argv(&e.get(&oc.id).unwrap().unwrap(), &reg()), vec!["/x/opencode", "--session", "ses_1"]);
        let mut cx = mk(SessionKind::Codex, vec!["/x/codex", "hi"]);
        assert_eq!(resume_argv(&cx, &reg()), cx.argv, "no id yet ⇒ relaunch, not resume");
        cx.agent_session_id = Some("t-9".into());
        assert_eq!(resume_argv(&cx, &reg()), vec!["/x/codex", "resume", "t-9"]);
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
        assert_eq!(resume_argv(&s, &reg()), vec!["claude", "--resume", "u"]);
        s.kind = SessionKind::Codex;
        assert_eq!(
            resume_argv(&s, &reg()),
            vec!["claude", "resume", "u"],
            "argv[0] is reused, the restore verb is per kind"
        );
        s.agent_session_id = None;
        assert_eq!(resume_argv(&s, &reg()), s.argv, "no id ⇒ relaunch");
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
                    agent_session_id: (kind == SessionKind::Claude).then(|| "u".to_string()),
                    kind: Some(kind),
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

    /// th-4f7866: two daemons sharing one flow.db — supervision only touches
    /// rows this daemon owns. A foreign daemon's live pane is not on this
    /// daemon's tmux server, so before the `owner` column a tick here marked
    /// it `dead · process vanished` and raced to relaunch it.
    #[test]
    fn supervision_skips_rows_owned_by_another_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let mine = tmux::socket_name();
        let mk = |owner: Option<&str>, sock: Option<&str>| {
            e.with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Shell),
                    argv: vec!["x".into()],
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    tmux_session: Some("fs-not-a-real-pane".into()),
                    tmux_socket: sock.map(str::to_string),
                    owner: owner.map(str::to_string),
                    ..Default::default()
                })
            })
            .unwrap()
        };
        // Another daemon's row — even one whose pane sits on a server of my name.
        let theirs = mk(Some("other-daemon"), Some(&mine));
        // A pre-column row on a foreign server: owned by that server's daemon.
        let legacy_foreign = mk(None, Some("flow-4f7866-not-mine"));
        // Mine, with the pane parked on the app's server (`--tmux-socket`).
        let mine_parked = mk(Some(&mine), Some("flow-4f7866-not-mine"));
        // A pre-column row on my server.
        let legacy_mine = mk(None, Some(&mine));

        assert!(!owned_here(&theirs) && !owned_here(&legacy_foreign));
        assert!(owned_here(&mine_parked) && owned_here(&legacy_mine));

        e.supervise_tick().unwrap();

        for s in [&theirs, &legacy_foreign] {
            assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Starting, "{}: not ours — untouched", s.id);
        }
        // Owned rows are still supervised: no such pane exists, so they die.
        for s in [&mine_parked, &legacy_mine] {
            let s = e.get(&s.id).unwrap().unwrap();
            assert_eq!(s.state, SessionState::Dead, "{}: ours — supervised", s.id);
            assert!(s.attention.unwrap().detail.unwrap().contains("process vanished"));
        }
    }

    /// A throwaway git repo with one commit, as the project (main checkout).
    fn git_project(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git(dir, &args).unwrap();
        }
    }

    fn done_row(e: &Engine, project: &Path, wt: &Path, branch: Option<&str>, pearl: Option<&str>) -> Session {
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Shell),
                    project: project.to_string_lossy().into(),
                    worktree: wt.to_string_lossy().into(),
                    branch: branch.map(str::to_string),
                    pearl_id: pearl.map(str::to_string),
                    ..Default::default()
                })
            })
            .unwrap();
        e.set_state(&s.id, SessionState::Done, None).unwrap().unwrap()
    }

    /// th-e126cc: `flow.close` refuses a dirty or unmerged worktree with
    /// nothing touched, removes worktree + branch once merged, and drops the
    /// row with a `flow.session.removed` broadcast.
    #[test]
    fn close_refuses_dirty_or_unmerged_then_removes_the_merged_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        git_project(&project);
        let e = engine(tmp.path());
        let mut rx = e.subscribe();
        let wt = Engine::create_worktree(&project, "th-e126cc", "x", "HEAD").unwrap();
        assert!(wt.is_dir() && wt != project);
        let s = done_row(&e, &project, &wt, Some("th-e126cc-x"), None);

        // Dirty: refused, nothing touched.
        std::fs::write(wt.join("scratch.txt"), "wip").unwrap();
        let err = e.close(&s.id, false, true, false).unwrap_err().to_string();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert!(wt.is_dir() && e.get(&s.id).unwrap().is_some());
        std::fs::remove_file(wt.join("scratch.txt")).unwrap();

        // Unmerged: refused, nothing touched.
        git(&wt, &["commit", "-q", "--allow-empty", "-m", "work"]).unwrap();
        let err = e.close(&s.id, false, true, false).unwrap_err().to_string();
        assert!(err.contains("not merged"), "{err}");
        assert!(wt.is_dir() && e.get(&s.id).unwrap().is_some());

        // Merged into the project: worktree + branch go, row goes, frame goes out.
        git(&project, &["merge", "-q", "--ff-only", "th-e126cc-x"]).unwrap();
        let out = e.close(&s.id, false, true, false).unwrap();
        assert_eq!(out.worktree_removed.as_deref(), Some(wt.to_string_lossy().as_ref()));
        assert_eq!(out.branch_deleted.as_deref(), Some("th-e126cc-x"));
        assert!(out.pearl_closed.is_none(), "no pearl asked for");
        assert!(!wt.exists(), "worktree removed");
        assert_eq!(git(&project, &["branch", "--list", "th-e126cc-x"]).unwrap(), "", "branch deleted");
        assert!(e.get(&s.id).unwrap().is_none(), "row removed");
        let mut removed = false;
        while let Ok(f) = rx.try_recv() {
            if matches!(&f, ServerFrame::SessionRemoved { id } if *id == s.id) {
                removed = true;
            }
        }
        assert!(removed, "flow.session.removed broadcast");
    }

    /// th-e126cc: `force` removes a dirty, unmerged worktree; the main checkout
    /// is never removed; a live row is killed first.
    #[test]
    fn close_force_removes_unmerged_and_never_touches_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        git_project(&project);
        let e = engine(tmp.path());
        let wt = Engine::create_worktree(&project, "th-e126cc", "y", "HEAD").unwrap();
        git(&wt, &["commit", "-q", "--allow-empty", "-m", "unmerged"]).unwrap();
        std::fs::write(wt.join("dirty.txt"), "x").unwrap();
        let s = done_row(&e, &project, &wt, None, None); // branch resolved from HEAD
        let out = e.close(&s.id, false, true, true).unwrap();
        assert!(out.worktree_removed.is_some() && out.branch_deleted.as_deref() == Some("th-e126cc-y"));
        assert!(!wt.exists());

        // The main checkout itself: `remove_worktree` is a no-op, the row still goes.
        let s = done_row(&e, &project, &project, Some("main"), None);
        let out = e.close(&s.id, false, true, true).unwrap();
        assert!(out.worktree_removed.is_none() && out.branch_deleted.is_none());
        assert!(project.is_dir() && e.get(&s.id).unwrap().is_none());

        // A live (never launched) row is killed, then removed.
        let live = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Shell),
                    project: project.to_string_lossy().into(),
                    worktree: project.to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap();
        assert!(!live.state.is_terminal());
        e.close(&live.id, false, false, false).unwrap();
        assert!(e.get(&live.id).unwrap().is_none());
        assert!(e.close("fs-nope", false, false, false).is_err());
    }

    /// th-e126cc: `close_pearl` runs `th pearls close <id>` in the project,
    /// and a failing `th` aborts before the row is dropped.
    #[test]
    #[cfg(unix)]
    fn close_closes_the_pearl_through_th() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        git_project(&project);
        let log = tmp.path().join("th.log");
        let fake = tmp.path().join("th");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display())).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("SMOOTH_TH_BIN", &fake);
        let e = engine(tmp.path());
        let s = done_row(&e, &project, &project, None, Some("th-abc123"));
        let out = e.close(&s.id, true, false, false).unwrap();
        assert_eq!(out.pearl_closed.as_deref(), Some("th-abc123"));
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "pearls close th-abc123");
        assert!(e.get(&s.id).unwrap().is_none());

        // No pearl on the row: nothing to close, still removed.
        let s = done_row(&e, &project, &project, None, None);
        assert!(e.close(&s.id, true, false, false).unwrap().pearl_closed.is_none());

        // th fails: the row stays.
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let s = done_row(&e, &project, &project, None, Some("th-abc123"));
        assert!(e.close(&s.id, true, false, false).is_err());
        assert!(e.get(&s.id).unwrap().is_some(), "nothing dropped when th refuses");
        std::env::remove_var("SMOOTH_TH_BIN");
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

    /// th-f4073b: the first porcelain line arrives without its leading space.
    #[test]
    fn dirty_path_survives_the_trimmed_first_line() {
        assert_eq!(dirty_path("M apps/smoothflow/x.swift").as_deref(), Some("apps/smoothflow/x.swift"));
        assert_eq!(dirty_path(" M apps/smoothflow/x.swift").as_deref(), Some("apps/smoothflow/x.swift"));
        assert_eq!(dirty_path("?? new.rs").as_deref(), Some("new.rs"));
        assert_eq!(dirty_path("MM both.rs").as_deref(), Some("both.rs"));
        assert_eq!(dirty_path("R  old.rs -> new.rs").as_deref(), Some("old.rs -> new.rs"));
        assert_eq!(dirty_path(""), None);
        assert_eq!(dirty_path("??"), None);
    }

    /// Tests that point `SMOOTH_TH_BIN` somewhere serialize on this — the env
    /// is process-wide and cargo runs tests in parallel.
    static TH_BIN_LOCK: Mutex<()> = Mutex::new(());

    // ── adoption (th-c103c1) ──────────────────────────────────────────────

    #[test]
    fn adopt_setting_env_beats_the_store_and_defaults_off() {
        assert!(!adopt_setting(None, None), "adoption is opt-in");
        assert!(adopt_setting(None, Some("1")));
        assert!(adopt_setting(None, Some("true")));
        assert!(!adopt_setting(None, Some("0")));
        assert!(!adopt_setting(None, Some("")));
        assert!(adopt_setting(Some("on"), Some("0")), "the env wins");
        assert!(!adopt_setting(Some("off"), Some("1")), "…in both directions");
        assert!(adopt_setting(Some("  "), Some("yes")), "a blank env is not a value");
    }

    #[test]
    fn harness_names_map_onto_manifest_kinds() {
        let r = reg();
        assert_eq!(kind_for_harness(&r, "claude-code"), Some(SessionKind::Claude), "what the Claude hooks send");
        assert_eq!(kind_for_harness(&r, "claude"), Some(SessionKind::Claude));
        assert_eq!(kind_for_harness(&r, "CODEX"), Some(SessionKind::Codex));
        assert_eq!(kind_for_harness(&r, "opencode"), Some(SessionKind::Opencode));
        assert_eq!(kind_for_harness(&r, "th-code"), Some("th-code".parse().unwrap()));
        assert_eq!(kind_for_harness(&r, "cursor"), None, "no manifest, no adoption");
        assert_eq!(kind_for_harness(&r, "-code"), None);
        assert_eq!(kind_for_harness(&r, ""), None);
    }

    #[test]
    fn only_projects_the_fleet_already_works_in_are_known() {
        let known = vec!["/dev/smooai".to_string()];
        assert!(project_is_known("/dev/smooth", "/dev/smooth", &known), "the daemon's own workspace");
        assert!(project_is_known("/dev/smooai", "/dev/smooth", &known), "a project with sessions");
        assert!(!project_is_known("/dev/stranger", "/dev/smooth", &known));
        // The symlink trap this guard fell into first: git answers with the
        // resolved path, so both sides go through `canon`.
        let tmp = tempfile::tempdir().unwrap();
        let raw = tmp.path().to_string_lossy().into_owned();
        assert!(project_is_known(&canon(&raw), &canon(&raw), &[]));
        assert_eq!(canon("/definitely/not/a/path"), "/definitely/not/a/path", "unresolvable falls back");
    }

    #[test]
    fn adoption_guards_each_refuse_for_their_own_reason() {
        let git = crate::infer::infer(
            Path::new("/dev/smooth"),
            &crate::infer::GitFacts {
                toplevel: Some("/dev/smooth".into()),
                common_dir: Some("/dev/smooth/.git".into()),
                branch: Some("th-c103c1-zf".into()),
            },
            &crate::infer::PearlFacts::default(),
        );
        let no_git = crate::infer::infer(Path::new("/tmp/x"), &crate::infer::GitFacts::default(), &crate::infer::PearlFacts::default());
        let claude = Some(SessionKind::Claude);
        assert_eq!(adoptable("Stop", "u1", false, Some(&git), claude.clone(), true), Err(AdoptRefusal::Disabled));
        assert_eq!(adoptable("Stop", "  ", true, Some(&git), claude.clone(), true), Err(AdoptRefusal::NoSessionId));
        assert_eq!(
            adoptable("PermissionRequest", "u1", true, Some(&git), claude.clone(), true),
            Err(AdoptRefusal::PermissionFirst),
            "adopting here would hold a plain terminal open for a decision nobody can see"
        );
        assert_eq!(adoptable("Stop", "u1", true, Some(&git), None, true), Err(AdoptRefusal::UnknownHarness));
        assert_eq!(adoptable("Stop", "u1", true, Some(&no_git), claude.clone(), true), Err(AdoptRefusal::NotGit));
        assert_eq!(adoptable("Stop", "u1", true, None, claude.clone(), true), Err(AdoptRefusal::NotGit));
        assert_eq!(
            adoptable("Stop", "u1", true, Some(&git), claude.clone(), false),
            Err(AdoptRefusal::UnknownProject)
        );
        assert_eq!(adoptable("Stop", "u1", true, Some(&git), claude, true), Ok(SessionKind::Claude));
    }

    #[test]
    fn an_adopted_session_goes_dead_only_after_a_long_silence() {
        let now = Utc::now();
        assert!(!adopted_is_stale(now, now));
        assert!(!adopted_is_stale(now - chrono::Duration::hours(5), now));
        assert!(adopted_is_stale(now - chrono::Duration::hours(7), now));
    }

    /// A git repo at `dir` on `branch`, with one commit.
    fn git_repo(dir: &Path, branch: &str) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        run(&["checkout", "-q", "-b", branch]);
    }

    fn hook_from(session: &str, event: &str, cwd: &Path) -> HookEvent {
        HookEvent {
            harness: "claude-code".into(),
            event: event.into(),
            session_id: session.into(),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            payload: json!({}),
        }
    }

    /// An engine whose workspace is a real repo, with `th` pointed nowhere so
    /// inference never touches the developer's pearl store.
    fn adopting_engine(tmp: &Path, branch: &str, on: bool) -> Engine {
        git_repo(tmp, branch);
        let e = engine(tmp);
        e.set_adopt(on).unwrap();
        e
    }

    #[test]
    fn a_plain_session_is_not_adopted_unless_the_user_opted_in() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-c103c1-zf", false);
        let reply = e.hook(hook_from("plain-1", "UserPromptSubmit", tmp.path())).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        assert!(matches!(reply, HookReply::Immediate(_)));
        assert!(e.list().unwrap().is_empty(), "adoption is off by default");
    }

    #[test]
    fn a_plain_session_is_adopted_with_its_pearl_branch_and_worktree() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-c103c1-zero-friction", true);
        e.hook(hook_from("plain-2", "UserPromptSubmit", tmp.path())).unwrap();
        // A second event must bind to the SAME row, not adopt again.
        e.hook(hook_from("plain-2", "PostToolUse", tmp.path())).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");

        let rows = e.list().unwrap();
        assert_eq!(rows.len(), 1, "one harness session is one row");
        let s = &rows[0];
        assert!(s.adopted);
        assert_eq!(s.kind, SessionKind::Claude, "`claude-code` is the claude manifest");
        assert_eq!(s.agent_session_id.as_deref(), Some("plain-2"));
        assert_eq!(s.branch.as_deref(), Some("th-c103c1-zero-friction"));
        assert_eq!(s.pearl_id.as_deref(), Some("th-c103c1"), "inferred off the branch");
        assert_eq!(s.title, "th-c103c1-zero-friction");
        assert!(s.argv.is_empty(), "we did not launch it");
        assert_eq!(s.tmux_session, None);
        assert_eq!(s.state, SessionState::Working);
    }

    #[test]
    fn adoption_refuses_strangers_non_repos_and_permission_requests() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-aaa111-x", true);
        // Another repo entirely.
        let other = tempfile::tempdir().unwrap();
        git_repo(other.path(), "th-bbb222-y");
        e.hook(hook_from("stranger", "Stop", other.path())).unwrap();
        // Not a repo at all.
        let plain = tempfile::tempdir().unwrap();
        e.hook(hook_from("nogit", "Stop", plain.path())).unwrap();
        // A permission request is never the first thing adopted.
        e.hook(hook_from("perm", "PermissionRequest", tmp.path())).unwrap();
        // No cwd at all.
        let mut bare = hook_from("nocwd", "Stop", tmp.path());
        bare.cwd = None;
        e.hook(bare).unwrap();
        // An unknown harness.
        let mut cursor = hook_from("cursor-1", "Stop", tmp.path());
        cursor.harness = "cursor".into();
        e.hook(cursor).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        assert!(e.list().unwrap().is_empty(), "every guard refused");
    }

    #[test]
    fn a_refusal_is_cached_so_the_shell_outs_happen_once() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-ccc333-z", true);
        let plain = tempfile::tempdir().unwrap();
        e.hook(hook_from("nogit", "Stop", plain.path())).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        assert_eq!(e.rt().adopt_refused.get("nogit").copied(), Some(AdoptRefusal::NotGit));
        // …and turning adoption on clears the cache, so the user's decision
        // takes effect without restarting the daemon.
        e.set_adopt(true).unwrap();
        assert!(e.rt().adopt_refused.is_empty());
    }

    #[test]
    fn an_adopted_session_ends_on_its_own_hook_and_refuses_engine_lifecycle() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-ddd444-w", true);
        e.hook(hook_from("plain-3", "UserPromptSubmit", tmp.path())).unwrap();
        let id = e.list().unwrap()[0].id.clone();

        let kill = e.kill(&id, false).unwrap_err().to_string();
        assert!(kill.contains("adopted"), "we do not own that process: {kill}");
        let attach = e.attach(&id, 80, 24).unwrap_err().to_string();
        assert!(attach.contains("no pane to attach"), "{attach}");
        let close = e.close(&id, false, false, false).unwrap_err().to_string();
        assert!(close.contains("adopted"), "{close}");

        // Its own SessionEnd is the only end-of-life signal there is.
        e.hook(hook_from("plain-3", "SessionEnd", tmp.path())).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        assert_eq!(e.get(&id).unwrap().unwrap().state, SessionState::Done);
        // Terminal now, so closing it out is allowed.
        e.close(&id, false, false, false).unwrap();
        assert!(e.get(&id).unwrap().is_none());
    }

    #[test]
    fn supervision_leaves_a_fresh_adopted_row_alone_and_buries_a_silent_one() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/binary");
        let tmp = tempfile::tempdir().unwrap();
        let e = adopting_engine(tmp.path(), "th-eee555-v", true);
        e.hook(hook_from("plain-4", "UserPromptSubmit", tmp.path())).unwrap();
        std::env::remove_var("SMOOTH_TH_BIN");
        let s = e.list().unwrap()[0].clone();
        e.supervise_tick().unwrap();
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Working, "no pane is not a death");

        let stale = Session {
            updated_at: Utc::now() - chrono::Duration::hours(9),
            ..s
        };
        e.supervise_one(&stale, Utc::now()).unwrap();
        let after = e.get(&stale.id).unwrap().unwrap();
        assert_eq!(after.state, SessionState::Dead);
        assert_eq!(after.attention.unwrap().reason, "crashed");
    }

    #[test]
    fn rows_another_daemon_changed_are_rebroadcast() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        // First tick only takes the watermark.
        e.supervise_tick().unwrap();
        let mut rx = e.subscribe();
        // A row owned by SOMEBODY ELSE's daemon, written straight to the
        // shared store — exactly what the other daemon's hook handler does.
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Claude),
                    owner: Some("some-other-daemon".into()),
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap();
        e.supervise_tick().unwrap();
        let frame = rx.try_recv().expect("the other daemon's row is re-broadcast");
        match frame {
            ServerFrame::Session { session } => assert_eq!(session.id, s.id),
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[test]
    fn handoff_degrades_without_th_or_gh() {
        let _g = TH_BIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // The request names the socket (th-d33afa) — no env, no lock.
        let sock = format!("flow-e-{}", std::process::id());
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let mut rx = e.subscribe();
        let s = e
            .new_session(NewRequest {
                kind: SessionKind::Shell,
                worktree: Some(tmp.path().to_string_lossy().into()),
                argv: Some(vec!["sh".into(), "-c".into(), "echo FLOW-READY; cat".into()]),
                title: Some("t".into()),
                tmux_socket: Some(sock.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.tmux_socket.as_deref(), Some(sock.as_str()), "the row records its socket");
        assert!(tmux::session_alive(&sock, &s.id) && !tmux::session_alive(&tmux::socket_name(), &s.id));
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
        // The steer is on the event stream (user), after the shell's idle line.
        let events = e.events(&s.id).unwrap();
        assert_eq!(events.last().map(|ev| (ev.kind, ev.text.as_str())), Some((EventKind::User, "hello-flow")));
        assert_eq!(events.last().unwrap().event_id, format!("{}-{}", s.id, events.len()));
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
        assert!(!tmux::session_alive(&sock, &s.id));
        e.remove(&s.id).unwrap();

        // Death detection: a session whose command exits non-zero is dead
        // (shell kind ⇒ no resume).
        let s2 = e
            .new_session(NewRequest {
                kind: SessionKind::Shell,
                worktree: Some(tmp.path().to_string_lossy().into()),
                argv: Some(vec!["sh".into(), "-c".into(), "exit 3".into()]),
                tmux_socket: Some(sock.clone()),
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
        tmux::kill_server(&sock);
    }

    /// th-d33afa: hooks and state changes feed the per-session event stream
    /// (the phone's Chat tab), buffered in the store for replay on attach.
    #[test]
    fn hooks_and_state_changes_feed_the_event_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some(SessionKind::Claude),
                    agent_session_id: Some("uuid-ev".into()),
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
            session_id: "uuid-ev".into(),
            cwd: None,
            payload,
        };
        e.hook(ev("UserPromptSubmit", json!({"prompt":"fix it"}))).unwrap();
        e.hook(ev("PreToolUse", json!({"tool_name":"Bash","tool_input":{"command":"ls"}}))).unwrap();
        e.hook(ev("Stop", json!({"last_assistant_message":"done"}))).unwrap();
        let reply = e
            .hook(ev("PermissionRequest", json!({"tool_name":"Bash","tool_input":{"command":"rm x"}})))
            .unwrap();
        let HookReply::Pending { request_id, .. } = reply else {
            panic!("expected pending")
        };
        e.approve(&s.id, &request_id, Decision::Deny).unwrap();

        let lines: Vec<(EventKind, String)> = e.events(&s.id).unwrap().into_iter().map(|x| (x.kind, x.text)).collect();
        assert_eq!(
            lines,
            vec![
                (EventKind::User, "fix it".into()),
                (EventKind::System, "working".into()),
                (EventKind::Tool, "● Bash(ls)".into()),
                (EventKind::Agent, "done".into()),
                (EventKind::System, "idle".into()),
                (EventKind::System, "needs_you · permission: Bash: rm x".into()),
                (EventKind::User, "approve: deny".into()),
                (EventKind::System, "working".into()),
            ]
        );
        // Every line was also broadcast as flow.event, ids monotonic.
        let mut ids = Vec::new();
        while let Ok(f) = rx.try_recv() {
            if let ServerFrame::Event { id, event } = f {
                assert_eq!(id, s.id);
                ids.push(event.event_id);
            }
        }
        let want: Vec<String> = (1..=lines.len()).map(|n| format!("{}-{n}", s.id)).collect();
        assert_eq!(ids, want);
        // A store failure never fails the caller (unknown session id).
        e.event("fs-ghost", EventKind::System, "x");
        assert!(e.events("fs-ghost").unwrap().is_empty());
    }

    /// th-0f6126: a native harness (th code) reports its own turns; the
    /// manifest's event_map drives state and the row reads VIA native.
    #[test]
    fn native_harness_events_map_through_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let s = e
            .with_store(|st| {
                st.create(NewSession {
                    kind: Some("th-code".parse().unwrap()),
                    agent_session_id: Some("fs-native".into()),
                    project: tmp.path().to_string_lossy().into(),
                    worktree: tmp.path().to_string_lossy().into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let ev = |event: &str, payload: Value| HookEvent {
            harness: "th-code".into(),
            event: event.into(),
            session_id: "fs-native".into(),
            cwd: None,
            payload,
        };
        e.hook(ev("turn_start", json!({}))).unwrap();
        let row = e.get(&s.id).unwrap().unwrap();
        assert_eq!(row.state, SessionState::Working);
        assert_eq!(row.state_source, "native");
        e.hook(ev("turn_end", json!({}))).unwrap();
        let row = e.get(&s.id).unwrap().unwrap();
        assert_eq!(row.state, SessionState::Idle);
        assert!(row.unread);
        // Claude's names mean nothing to a mapped harness.
        e.hook(ev("UserPromptSubmit", json!({}))).unwrap();
        assert_eq!(e.get(&s.id).unwrap().unwrap().state, SessionState::Idle);
        // A generic needs_you through the map carries reason + message.
        let m = Manifest::parse(
            "name=\"x\"\n[binary]\nnames=[\"x\"]\n[launch]\nargv=[\"{prompt}\"]\n[state.hooks.event_map]\nask=\"needs_you\"\nbye=\"ended\"\nmeh=\"ignore\"",
        )
        .unwrap();
        let mk = |event: &str, payload: Value| HookEvent {
            harness: "x".into(),
            event: event.into(),
            session_id: "s".into(),
            cwd: None,
            payload,
        };
        match mapped_outcome(&m, &mk("ask", json!({"reason":"permission","message":"rm -rf"}))) {
            HookOutcome::NeedsYou(a) => {
                assert_eq!(a.reason, "permission");
                assert_eq!(a.detail.as_deref(), Some("rm -rf"));
            }
            other => panic!("{other:?}"),
        }
        match mapped_outcome(&m, &mk("ask", json!({"tool_name":"Bash","tool_input":{"command":"ls"}}))) {
            HookOutcome::NeedsYou(a) => {
                assert_eq!(a.reason, "question");
                assert_eq!(a.detail.as_deref(), Some("Bash: ls"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(mapped_outcome(&m, &mk("bye", json!({}))), HookOutcome::Ended);
        assert_eq!(mapped_outcome(&m, &mk("meh", json!({}))), HookOutcome::None);
        assert_eq!(mapped_outcome(&m, &mk("Stop", json!({}))), HookOutcome::None, "unlisted ⇒ nothing");
    }

    /// th-0f6126: prefs persist in flow.db, order/hide the list, reach
    /// `flow.hello` and broadcast `flow.harnesses`.
    /// th-473294: a key press addresses a pane, so an unknown session is an
    /// error, not a silent no-op.
    #[test]
    fn send_key_needs_a_known_session() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let err = e.send_key("fs-nope", "Enter").unwrap_err().to_string();
        assert!(err.contains("no such session"), "{err}");
    }

    #[test]
    fn harness_prefs_persist_order_and_hide() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let names = |v: &[HarnessInfo]| v.iter().map(|h| h.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&e.harnesses(true).unwrap()), ["claude", "opencode", "codex", "th-code"]);
        let mut rx = e.subscribe();
        let all = e.set_harness_prefs(Some(vec!["th-code".into()]), Some(vec!["codex".into()])).unwrap();
        assert_eq!(names(&all), ["th-code", "claude", "opencode", "codex"]);
        assert!(all[3].hidden);
        match rx.try_recv().unwrap() {
            ServerFrame::Harnesses { harnesses } => assert_eq!(names(&harnesses), ["th-code", "claude", "opencode"]),
            other => panic!("{other:?}"),
        }
        // Visible list + hello omit the hidden one; a partial PUT keeps the other half.
        assert_eq!(names(&e.harnesses(false).unwrap()), ["th-code", "claude", "opencode"]);
        match e.hello().unwrap() {
            ServerFrame::Hello { harnesses, .. } => assert_eq!(names(&harnesses), ["th-code", "claude", "opencode"]),
            other => panic!("{other:?}"),
        }
        let all = e.set_harness_prefs(None, Some(vec![])).unwrap();
        assert_eq!(names(&all), ["th-code", "claude", "opencode", "codex"]);
        assert!(all.iter().all(|h| !h.hidden));
        // Persisted: a fresh engine on the same db sees the order.
        let e2 = engine(tmp.path());
        assert_eq!(e2.harness_prefs().unwrap().order, vec!["th-code".to_string()]);
        // Unknown names are refused, nothing changes.
        let err = e.set_harness_prefs(None, Some(vec!["cursor".into()])).unwrap_err().to_string();
        assert!(err.contains("`cursor` is not a known harness"), "{err}");
        assert!(e.harness_prefs().unwrap().hidden.is_empty());
        // A user manifest under the engine's home shows up with its origin.
        let dir = tmp.path().join("home/.smooth/harnesses");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("nosuchtool.toml"),
            "name=\"nosuchtool\"\n[binary]\nnames=[\"nosuchtool-xyzzy\"]\n[launch]\nargv=[\"{prompt}\"]\n",
        )
        .unwrap();
        let all = e.harnesses(true).unwrap();
        let user = all.iter().find(|h| h.name == "nosuchtool").unwrap();
        assert_eq!(user.origin, "user");
        assert!(!user.installed);
        assert!(user.reason.as_deref().unwrap().contains("`nosuchtool-xyzzy` not found on PATH"));
    }

    /// th-0f6126: an unknown kind is refused before anything is created.
    #[test]
    fn new_session_refuses_an_unknown_harness() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let err = e
            .new_session(NewRequest {
                kind: "aider".parse().unwrap(),
                worktree: Some(tmp.path().to_string_lossy().into_owned()),
                ..Default::default()
            })
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown harness kind `aider`"), "{err}");
        assert!(e.list().unwrap().is_empty());
    }
}
