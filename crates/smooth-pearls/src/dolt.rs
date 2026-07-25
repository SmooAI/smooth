//! smooth-dolt subprocess wrapper.
//!
//! Provides a clean Rust interface to the `smooth-dolt` Go binary for
//! all Dolt operations (init, SQL, commit, push, pull, log, remote, gc).
//! The binary is located once at startup and reused for all calls.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::dolt_server::SmoothDoltServer;

/// Default wallclock bound on a remote sync (`smooth-dolt push` /
/// `pull`). The Dolt remote sync moves `refs/dolt/data`; the sync child
/// holds the noms `LOCK` for the whole transfer. If the network to the
/// remote stalls, the lock is held indefinitely and EVERY other writer
/// of this store gets `Error 1105: cannot update manifest: database is
/// read only` (reads still work). Bounding the sync means a network
/// stall can NEVER wedge local writes *forever* — on timeout we kill the
/// child, the OS releases the lock, and the caller sees a retryable
/// "sync timed out" error instead of a permanent wedge.
///
/// This bound is a wallclock ceiling, NOT a stall detector. It cannot
/// tell a healthy-but-slow transfer from a dead socket, because
/// `smooth-dolt push` runs `CALL DOLT_PUSH(...)` as a single synchronous
/// SQL call that emits NO progress on stdout/stderr — a legitimate large
/// upload is byte-silent for its entire duration (pearl th-e814a9: a
/// 303M smooai pearl store re-uploaded a post-gc 114M oldgen table in
/// ~150s over a home uplink, emitting nothing). So the ceiling must be
/// generous enough to clear a real large push; at 30s every retry
/// restarted the same transfer and hit the same wall, making the store
/// impossible to sync. 300s clears observed large pushes while still
/// capping a genuine dead-socket wedge at 5 minutes. Normal incremental
/// pushes finish in ~10s and are unaffected.
///
/// Override with `SMOOTH_DOLT_SYNC_TIMEOUT_SECS`. A value of `0`
/// disables the bound (restores the old unbounded behavior) for callers
/// that genuinely want to block on a slow-but-progressing transfer.
const DEFAULT_SYNC_TIMEOUT_SECS: u64 = 300;

/// Resolve the remote-sync timeout from the environment, falling back to
/// [`DEFAULT_SYNC_TIMEOUT_SECS`]. `None` means "no bound" (env set to
/// `0` or a value that doesn't parse as a positive integer where the
/// caller explicitly passed `0`).
fn sync_timeout() -> Option<Duration> {
    std::env::var("SMOOTH_DOLT_SYNC_TIMEOUT_SECS").map_or_else(|_| Some(Duration::from_secs(DEFAULT_SYNC_TIMEOUT_SECS)), |raw| parse_sync_timeout(&raw))
}

/// Pure parser for the `SMOOTH_DOLT_SYNC_TIMEOUT_SECS` value. Factored
/// out so the env-parsing logic is unit-testable without touching real
/// process env. `0` → `None` (unbounded). A non-numeric / negative
/// value → fall back to the default bound (fail safe: we never silently
/// drop the protection because someone typo'd the env var).
fn parse_sync_timeout(raw: &str) -> Option<Duration> {
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => Some(Duration::from_secs(DEFAULT_SYNC_TIMEOUT_SECS)),
    }
}

/// Default wallclock bound on a LOCAL one-shot (`sql`, `exec`, `log`,
/// `status`, `version`) — no network, no big transfer, so a healthy call
/// returns in tens of milliseconds. Anything still running after this is
/// wedged.
///
/// Pearl th-118847: without a bound, a wedged one-shot hung `th`
/// forever; the user Ctrl-C'd, the child was orphaned (Rust's `Command`
/// does not kill children on drop), and the orphan kept the store pinned
/// — every later write got `Error 1105: cannot update manifest: database
/// is read only`. Five of them had piled up. Bounding the call means the
/// child is SIGKILLed and reaped by the process that spawned it, so
/// there is nothing left to leak.
///
/// Remote-sync commands (`push`/`pull`/`clone`) are NOT bounded by this
/// — they have their own, far more generous [`sync_timeout`]. Override
/// with `SMOOTH_DOLT_QUERY_TIMEOUT_SECS`; `0` disables the bound.
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 120;

/// Stall note for an aborted remote sync. [`is_sync_timeout_err`] keys
/// off the "remote sync stalled" marker to classify the failure as
/// retryable — keep them in step.
const SYNC_STALL_HINT: &str = "remote sync stalled; killed child to release lock — retryable";

/// Stall note for an aborted LOCAL one-shot. Deliberately does NOT carry
/// the sync marker: this is not a network problem and is not deferrable.
const QUERY_STALL_HINT: &str = "local query stalled; killed the child so it can't leak and pin the store read-only — \
     re-run, and `th pearls doctor --reap` if writes still fail (SMOOTH_DOLT_QUERY_TIMEOUT_SECS to change the bound)";

/// Wallclock bound for a given smooth-dolt subcommand, or `None` for
/// "unbounded" (remote-sync commands, which [`SmoothDolt::run_cli_timed`]
/// bounds separately, and long local maintenance like `gc`).
fn one_shot_timeout(subcommand: &str) -> Option<Duration> {
    if !matches!(subcommand, "sql" | "exec" | "log" | "status" | "version") {
        return None;
    }
    std::env::var("SMOOTH_DOLT_QUERY_TIMEOUT_SECS").map_or_else(|_| Some(Duration::from_secs(DEFAULT_QUERY_TIMEOUT_SECS)), |raw| parse_query_timeout(&raw))
}

/// Pure parser for `SMOOTH_DOLT_QUERY_TIMEOUT_SECS`. Same fail-safe
/// shape as [`parse_sync_timeout`]: `0` → unbounded, garbage → default.
fn parse_query_timeout(raw: &str) -> Option<Duration> {
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => Some(Duration::from_secs(DEFAULT_QUERY_TIMEOUT_SECS)),
    }
}

/// Derive a stable, human-readable cache key (`owner_repo`) from a Dolt
/// git remote URL like `git+ssh://git@github.com/SmooAI/smooth.git`.
/// Returns `None` for anything that doesn't have at least an `owner/repo`
/// tail, so callers fall back to the per-worktree cache (no sharing).
fn shared_cache_key_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = trimmed.rsplit(['@', '/']).take(2).collect::<Vec<_>>();
    // rsplit yields [repo, owner]; need both to key uniquely per repo.
    let (repo, owner) = (path.first()?, path.get(1)?);
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() || owner.is_empty() {
        return None;
    }
    let sanitize = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
    Some(format!("{}_{}", sanitize(owner), sanitize(repo)))
}

/// Read `origin` from a pearl store's `.dolt/repo_state.json`.
fn origin_remote_url(dot_dolt: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dot_dolt.join("repo_state.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    json.get("remotes")?.get("origin")?.get("url")?.as_str().map(str::to_owned)
}

/// Point this worktree's git-remote cache at ONE shared, persistent
/// per-machine cache under `~/.smooth/git-remote-cache/<owner_repo>/`
/// (pearl th-20f330).
///
/// Dolt hardcodes the git-remote cache to the per-worktree DB dir
/// (`<db>/.dolt/git-remote-cache`, see dolt `DoltEnv::GitCacheRoot`), so
/// every fresh worktree / fresh clone starts cold and re-fetches the
/// FULL `refs/dolt/data` history from scratch — hundreds of MB, byte-
/// silent, while holding the single-writer noms LOCK, which wedges every
/// other agent read-only. Symlinking the per-worktree cache dir to a
/// shared location keyed by remote URL means the first bootstrap on a
/// machine is the ONLY cold fetch; every later worktree and sync is
/// incremental (seconds). Keeps all data in the repos — this only moves
/// the *cache*, never the store.
///
/// Best-effort: any error leaves the per-worktree cache in place (correct,
/// just slower). Unix-only symlink; on other platforms this is a no-op and
/// the per-worktree cache stands until the single-writer server (th-5f35a5)
/// lands the Windows path.
fn ensure_shared_git_cache(data_dir: &Path) {
    let Some(home) = dirs_next::home_dir() else { return };
    link_shared_git_cache(data_dir, &home.join(".smooth").join("git-remote-cache"));
}

/// Core of [`ensure_shared_git_cache`], with the shared-cache root passed
/// in so it's testable without touching the real `$HOME`.
fn link_shared_git_cache(data_dir: &Path, shared_root: &Path) {
    let dot_dolt = data_dir.join(".dolt");
    if !dot_dolt.is_dir() {
        return; // store not initialized yet — dolt will create .dolt first
    }
    let Some(key) = origin_remote_url(&dot_dolt).as_deref().and_then(shared_cache_key_from_url) else {
        return; // no usable remote → nothing to share against
    };
    let shared = shared_root.join(&key);
    let local = dot_dolt.join("git-remote-cache");

    // Already linked to the shared target — the common steady-state case.
    if std::fs::read_link(&local).is_ok_and(|t| t == shared) {
        return;
    }
    if std::fs::create_dir_all(&shared).is_err() {
        return;
    }

    match std::fs::symlink_metadata(&local) {
        Ok(md) if md.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&local); // stale symlink → repoint below
        }
        Ok(md) if md.is_dir() => {
            let shared_empty = std::fs::read_dir(&shared).is_ok_and(|mut d| d.next().is_none());
            let local_empty = std::fs::read_dir(&local).map_or(true, |mut d| d.next().is_none());
            if shared_empty && !local_empty {
                // Migrate this worktree's warm cache into the shared slot once.
                let _ = std::fs::remove_dir(&shared);
                if std::fs::rename(&local, &shared).is_err() {
                    let _ = std::fs::create_dir_all(&shared); // cross-device: give up sharing, keep local cold-safe
                    return;
                }
            } else if std::fs::remove_dir_all(&local).is_err() {
                return; // shared already warm; couldn't drop the redundant local copy
            }
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&local); // unexpected non-dir file
        }
        Err(_) => {} // doesn't exist yet → just create the symlink
    }

    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&shared, &local);
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_dir(&shared, &local);
}

/// Escape a string for splicing into a single-quoted SQL string literal
/// sent to `smooth-dolt exec`/`sql` (no prepared statements on this path).
///
/// Dolt speaks MySQL dialect, where **backslash is an escape character
/// inside string literals** — doubling quotes alone is broken: input
/// containing `\'` became `\''`, the backslash ate the first quote, and
/// the rest of the value was parsed as SQL (syntax error at best,
/// injection at worst; pearl th-944230). Order matters: backslashes
/// first, then quotes, then NUL bytes (which MySQL rejects raw).
///
/// This is the ONE escaping function for the workspace — every
/// SQL-string-building site (pearls, memories, messages, agents,
/// bigsmooth sessions) must route through it.
#[must_use]
pub fn sql_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''").replace('\0', "\\0")
}

/// Flags for [`SmoothDolt::push_with`].
///
/// `set_upstream` translates to Dolt's `-u` flag and is needed on the
/// first push to a fresh remote. `force` translates to `-f` and
/// overrides a remote whose history shares no common ancestor with
/// the local store (typically a stale empty `Initialize data
/// repository` commit left by an earlier `dolt init` somewhere else).
#[derive(Debug, Clone, Copy, Default)]
pub struct PushOpts {
    pub force: bool,
    pub set_upstream: bool,
}

/// Handle to the smooth-dolt binary. All Dolt operations go through this.
///
/// Two transports are supported:
///
/// 1. **CLI mode** (default — [`SmoothDolt::new`]): each method spawns a
///    fresh `smooth-dolt sql ...` subprocess via `Command::output`.
///    Works fine for short-lived commands like `th pearls list`.
/// 2. **Server mode** ([`SmoothDolt::from_server`]): each method round-
///    trips through a long-running `smooth-dolt serve` subprocess over a
///    Unix socket. The Big Smooth long-running daemon uses this to avoid
///    a known hang where the second `PearlStore::open` inside the same
///    process wedges the spawned smooth-dolt subprocess in
///    `pthread_cond_wait` (see pearl `th-1a61a7`). The server itself is
///    spawned at startup (synchronous code, before tokio handlers run)
///    where the underlying issue doesn't fire.
#[derive(Debug, Clone)]
pub struct SmoothDolt {
    /// Path to the smooth-dolt binary. Used in CLI mode.
    bin: PathBuf,
    /// Path to the Dolt data directory (e.g., `.smooth/dolt/`).
    data_dir: PathBuf,
    /// When set, route operations through this long-running server's
    /// socket instead of spawning per-call. The `Arc` lets multiple
    /// `SmoothDolt` clones (and their `PearlStore` parents) share the
    /// same server without each owning a copy of the spawned child.
    server: Option<Arc<SmoothDoltServer>>,
}

impl SmoothDolt {
    /// Create a handle pointing at the given data directory.
    ///
    /// If a long-running `smooth-dolt serve` is already running for
    /// this dir (e.g. the Big Smooth daemon spawned one at startup),
    /// attach to it via [`SmoothDoltServer::try_attach`] and use
    /// server mode. Otherwise fall back to per-call CLI mode —
    /// never spawns a new server from this path, so one-shot `th
    /// pearls X` commands stay cheap.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir: PathBuf = data_dir.into();
        if let Some(server) = SmoothDoltServer::try_attach(&data_dir) {
            tracing::debug!(data_dir = %data_dir.display(), "SmoothDolt::new attached to existing server");
            return Ok(Self::from_server(Arc::new(server), data_dir));
        }
        let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found. Run: scripts/build-smooth-dolt.sh")?;
        Ok(Self { bin, data_dir, server: None })
    }

    /// Always-CLI handle — used by initialization paths that need to
    /// run `dolt init` on a fresh directory before any server can
    /// reasonably attach. Bypasses the attach-or-spawn flow that
    /// [`SmoothDolt::new`] performs.
    pub fn new_cli_only(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found. Run: scripts/build-smooth-dolt.sh")?;
        Ok(Self {
            bin,
            data_dir: data_dir.into(),
            server: None,
        })
    }

    /// Create a server-mode handle that routes all operations through a
    /// long-running [`SmoothDoltServer`] instead of spawning per-call.
    /// `data_dir` is informational here (returned by [`Self::data_dir`]);
    /// the actual storage path is whatever was passed to
    /// [`SmoothDoltServer::spawn`].
    #[must_use]
    pub fn from_server(server: Arc<SmoothDoltServer>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            // The `bin` field is unused in server mode but kept for the
            // accessor; pick something reasonable rather than holding an
            // Option just for this case.
            bin: server.socket_path(),
            data_dir: data_dir.into(),
            server: Some(server),
        }
    }

    /// Wrap a server-mode op with one round of self-healing. Two
    /// classes of recoverable failure trigger a respawn + retry:
    ///
    /// 1. **Transport** ([`is_transport_err`]): broken-pipe, EOF,
    ///    connection-refused, timeout. Server is dead or unreachable.
    ///    Respawn via `ensure_healthy()` (probes first, only kicks
    ///    if unhealthy).
    /// 2. **Lock wedge** ([`is_lock_wedge_err`]): server alive and
    ///    answering ping, but every write returns `Error 1105:
    ///    cannot update manifest: database is read only`. Pearl
    ///    th-a97d1f: this happens when an earlier writer crashed
    ///    and left a stale LOCK file the live server is still
    ///    holding — `is_healthy()` passes (server pings) but the
    ///    db is wedged. Force-respawn picks it up clean.
    ///
    /// Anything else propagates so callers can react meaningfully
    /// — syntax errors, not-found, validation failures stay user-
    /// visible. Cap is one retry per call.
    fn run_with_self_heal<T>(server: &Arc<SmoothDoltServer>, op: impl Fn(&Arc<SmoothDoltServer>) -> Result<T>) -> Result<T> {
        match op(server) {
            Ok(v) => Ok(v),
            Err(e) if is_transport_err(&e) => {
                tracing::warn!(error = %e, "smooth-dolt op looked like a transport failure; respawning + retrying once");
                server.ensure_healthy().context("self-heal: ensure_healthy")?;
                op(server)
            }
            Err(e) if is_lock_wedge_err(&e) => {
                tracing::warn!(error = %e, "smooth-dolt op hit db-read-only lock flap; force-respawn once + backoff-retry");
                // Recover once (force-respawn a wedged child), then back
                // off + retry to wait out a live peer's push — same shared
                // path CLI mode uses.
                retry_on_lock_flap(
                    || op(server),
                    || {
                        if let Err(re) = server.force_respawn() {
                            tracing::warn!(error = %re, "force_respawn during lock-flap recovery failed");
                        }
                    },
                )
            }
            Err(e) => Err(e),
        }
    }

    /// Path to the Dolt data directory backing this handle.
    #[must_use]
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Underlying long-running server, if this handle is in server
    /// mode. Used by the host process to drive a background health-
    /// check loop that respawns the child on macOS-sleep wedges.
    #[must_use]
    pub fn server(&self) -> Option<&Arc<SmoothDoltServer>> {
        self.server.as_ref()
    }

    /// Create a handle with an explicit binary path (for testing).
    #[must_use]
    pub fn with_bin(bin: PathBuf, data_dir: PathBuf) -> Self {
        Self { bin, data_dir, server: None }
    }

    /// Initialize a new Dolt database at the data directory. Server mode
    /// is rejected — init must run before a server can serve the dir.
    pub fn init(&self) -> Result<String> {
        if self.server.is_some() {
            anyhow::bail!("init is not supported in server mode; init the dolt dir first, then spawn the server");
        }
        self.run_cli(&["init", &self.data_dir_str()])
    }

    /// Execute a SQL query and return parsed JSON results. In server
    /// mode the call is serialized through the single-writer queue
    /// (see [`SmoothDoltServer::with_client`]) so concurrent callers
    /// can't race the Dolt manifest lock.
    pub fn sql(&self, query: &str) -> Result<Vec<Value>> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.sql(query)));
        }
        let output = self.run_cli(&["sql", &self.data_dir_str(), "-q", query])?;
        if output.is_empty() || output == "null" {
            return Ok(Vec::new());
        }
        let parsed: Vec<Value> = serde_json::from_str(&output).with_context(|| format!("parse smooth-dolt sql output: {output}"))?;
        Ok(parsed)
    }

    /// Execute a SQL statement (INSERT/UPDATE/DELETE/CREATE). Returns raw output.
    /// In CLI mode, dispatches to `smooth-dolt exec` (uses db.Exec,
    /// commits writes) rather than `smooth-dolt sql` (db.Query, drops
    /// uncommitted writes when the subprocess exits — this was
    /// silently swallowing every `th pearls create` write before
    /// store.create's verify-after-create caught it as
    /// "pearl not found after create").
    pub fn exec(&self, statement: &str) -> Result<String> {
        if let Some(server) = &self.server {
            let rows = Self::run_with_self_heal(server, |s| s.with_client(|c| c.exec(statement)))?;
            return Ok(format!("{rows} rows affected"));
        }
        self.run_cli(&["exec", &self.data_dir_str(), "-q", statement])
    }

    /// `exec` without the auto-doctor self-heal retry — the read-only
    /// error propagates verbatim instead of triggering a kill+retry.
    /// Used by [`probe_writable`]: a diagnostic must report the store's
    /// real state, not change it as a side effect of looking.
    fn exec_no_heal(&self, statement: &str) -> Result<String> {
        self.run_cli_once(&["exec", &self.data_dir_str(), "-q", statement])
    }

    /// Stage all changes and commit with a message.
    pub fn commit(&self, message: &str) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.commit(message)));
        }
        self.run_cli(&["commit", &self.data_dir_str(), "-m", message])
    }

    /// Query the Dolt commit log. Returns vec of (hash, author, date, message).
    pub fn log(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        let output = if let Some(server) = &self.server {
            Self::run_with_self_heal(server, |s| s.with_client(|c| c.log(limit)))?
        } else {
            self.run_cli(&["log", &self.data_dir_str(), "-n", &limit.to_string()])?
        };
        let mut entries = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: "hash message (author) date" — passthrough as a
            // single string for now; callers that need structured fields
            // can split.
            entries.push((line.to_string(), String::new(), String::new(), String::new()));
        }
        Ok(entries)
    }

    /// Push to the configured Dolt remote (refs/dolt/data on git origin)
    /// using default flags. Equivalent to [`Self::push_with`] with all
    /// options off.
    pub fn push(&self) -> Result<String> {
        self.push_with(PushOpts::default())
    }

    /// Push to the configured Dolt remote with explicit options.
    ///
    /// First push to a fresh remote needs `set_upstream = true` (Dolt's
    /// `-u` flag) — without it the push fails with "no upstream branch".
    /// `force = true` (the underlying `-f` flag) overrides remote
    /// history; only useful when the remote contains an empty
    /// `Initialize data repository` commit from a stale init that
    /// shares no ancestor with the local store.
    ///
    /// The CLI auto-retries with `set_upstream` on the first push so
    /// callers don't have to know the flag exists; this method is
    /// surfaced for callers that want explicit control.
    pub fn push_with(&self, opts: PushOpts) -> Result<String> {
        // Server mode (Bigsmooth in-process pearls) doesn't expose
        // flags through the protocol. It also doesn't push, so the
        // bare command is the right shape there.
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.dolt("push")));
        }
        // Redirect the git-remote cache to the shared per-machine slot
        // before dolt opens it, so this push reuses the warm cache instead
        // of a cold full-history fetch (pearl th-20f330).
        ensure_shared_git_cache(&self.data_dir);
        let mut args: Vec<&str> = vec!["push"];
        let data_dir = self.data_dir_str();
        args.push(&data_dir);
        // smooth-dolt forwards trailing args after the data dir into
        // the underlying dolt push.
        if opts.force {
            args.push("-f");
        }
        if opts.set_upstream {
            args.push("-u");
            args.push("origin");
            args.push("main");
        }
        // Bound the remote sync so a network stall can't hold the noms
        // LOCK forever and wedge local writes into read-only.
        self.run_cli_timed(&args, sync_timeout())
    }

    /// Pull from the configured Dolt remote.
    pub fn pull(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.dolt("pull")));
        }
        // Warm-cache redirect before the remote op (pearl th-20f330).
        ensure_shared_git_cache(&self.data_dir);
        // Bounded like push — a stalled pull holds the same LOCK.
        self.run_cli_timed(&["pull", &self.data_dir_str()], sync_timeout())
    }

    /// Add a Dolt remote. CLI-only; the server protocol doesn't expose
    /// remote management because it's an administrative one-shot.
    ///
    /// SCP-style git URLs (`git@github.com:Org/repo.git`) are normalized
    /// to `git+ssh://` form first — see [`normalize_remote_url`]. Pearl
    /// th-c4441b.
    pub fn remote_add(&self, name: &str, url: &str) -> Result<String> {
        if self.server.is_some() {
            anyhow::bail!("remote_add is not supported in server mode; use the CLI directly");
        }
        let url = normalize_remote_url(url);
        self.run_cli(&["remote", &self.data_dir_str(), "add", name, &url])
    }

    /// List configured Dolt remotes. CLI-only (see `remote_add`).
    pub fn remote_list(&self) -> Result<String> {
        if self.server.is_some() {
            anyhow::bail!("remote_list is not supported in server mode; use the CLI directly");
        }
        self.run_cli(&["remote", &self.data_dir_str(), "list"])
    }

    /// Garbage collect — compact the database to minimize storage.
    pub fn gc(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.dolt("gc")));
        }
        self.run_cli(&["gc", &self.data_dir_str()])
    }

    /// Check the Dolt status (working set changes).
    pub fn status(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.dolt("status")));
        }
        self.run_cli(&["status", &self.data_dir_str()])
    }

    /// Get the version of the smooth-dolt binary.
    pub fn version(&self) -> Result<String> {
        let output = Command::new(&self.bin)
            .arg("version")
            .output()
            .with_context(|| format!("exec smooth-dolt version: {}", self.bin.display()))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The data directory as a string.
    fn data_dir_str(&self) -> String {
        self.data_dir.to_string_lossy().to_string()
    }

    /// Run a smooth-dolt command and return stdout (CLI mode).
    ///
    /// Uses `Stdio::null()` for stdin and stderr. The Go runtime inside
    /// smooth-dolt forks a long-lived dolt sql-server child that inherits
    /// the parent's stderr fd; if we connected stderr to a pipe, that
    /// inherited fd stayed open after the smooth-dolt parent exited and
    /// `Command::output()` waited for EOF on the pipe forever (observed on
    /// smoo-hub: 60s+ HTTP timeouts on `/api/projects` while the same
    /// command run from a TTY returned in 50ms). Discarding stderr breaks
    /// that inheritance chain. We still capture stdout because callers
    /// need the SQL result; on failure we surface a generic message
    /// instead of stderr — operators can re-run the underlying CLI for
    /// detail.
    fn run_cli(&self, args: &[&str]) -> Result<String> {
        // On the read-only lock-flap: recover once (Pearl th-49e37b — the
        // common CLI-mode cause is an orphaned `smooth-dolt serve` still
        // holding the LOCK after its socket was cleaned up; reap it), then
        // back off + retry to wait out a live peer's push. Both handled by
        // the shared `retry_on_lock_flap`.
        retry_on_lock_flap(
            || self.run_cli_once(args),
            || match auto_doctor_clear_orphan_server(&self.data_dir) {
                Ok(cleared) if cleared > 0 => {
                    tracing::warn!(
                        data_dir = %self.data_dir.display(),
                        cleared,
                        "smooth-dolt CLI hit read-only; cleared orphaned `smooth-dolt serve` PID(s)"
                    );
                }
                _ => {}
            },
        )
    }

    /// One-shot CLI invocation. Wrapped by `run_cli` with the
    /// auto-doctor retry — that's the public entry point. This bare
    /// version lives separately so the doctor's retry path can call
    /// it without re-entering the doctor and looping forever.
    ///
    /// Local one-shots (`sql`/`exec`/`log`/…) carry a wallclock bound —
    /// see [`one_shot_timeout`] and pearl th-118847: an unbounded hung
    /// child gets orphaned when the user gives up on `th` and Ctrl-Cs,
    /// and the orphan then pins the store read-only for everyone.
    fn run_cli_once(&self, args: &[&str]) -> Result<String> {
        let subcommand = args.first().copied().unwrap_or_default();
        let child = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("exec smooth-dolt {}: {}", args.join(" "), self.bin.display()))?;
        let output = wait_child_draining(child, one_shot_timeout(subcommand), &format!("smooth-dolt {subcommand}"), QUERY_STALL_HINT)?;

        if !output.status.success() {
            // Capture stderr inline so callers (and the operator log) get
            // a useful failure mode instead of the old "rerun the CLI for
            // stderr" cul-de-sac. Trim + clip to keep one-line callsites
            // readable.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_clip: String = stderr.trim().chars().take(300).collect();
            anyhow::bail!(
                "smooth-dolt {} failed (exit {}): {}",
                args.first().unwrap_or(&""),
                output.status.code().unwrap_or(-1),
                if stderr_clip.is_empty() { "(no stderr)" } else { stderr_clip.as_str() }
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a smooth-dolt command (CLI mode) with a wallclock bound,
    /// killing the child if it exceeds `timeout`. Used for remote-sync
    /// commands (`push` / `pull`) where a network stall would otherwise
    /// hold the noms `LOCK` forever and wedge every other writer into
    /// read-only (Pearl: dolt-sync-timeout-selfheal).
    ///
    /// `timeout = None` means "no bound" — falls straight through to the
    /// ordinary blocking [`Self::run_cli_once`].
    ///
    /// On timeout the child is SIGKILLed (it's a stuck git transfer; a
    /// graceful term would just have us wait longer for a process that's
    /// blocked on a dead socket), reaped to avoid a zombie, and a
    /// **retryable** error is returned — see [`is_sync_timeout_err`].
    /// Killing the child releases its hold on the noms LOCK, so local
    /// writes recover immediately; the sync itself can be retried later.
    fn run_cli_timed(&self, args: &[&str], timeout: Option<Duration>) -> Result<String> {
        // Untimed path shares run_cli's flap retry.
        let Some(timeout) = timeout else {
            return self.run_cli(args);
        };
        // Timed remote sync (push/pull): wait out a read-only flap too, so
        // a pull racing a peer's push doesn't fail fast. A stalled sync
        // returns a distinct `is_sync_timeout_err` (not a lock-wedge), so
        // that path is untouched by this retry.
        retry_on_lock_flap(
            || self.run_cli_timed_once(args, timeout),
            || {
                let _ = auto_doctor_clear_orphan_server(&self.data_dir);
            },
        )
    }

    fn run_cli_timed_once(&self, args: &[&str], timeout: Duration) -> Result<String> {
        let child = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn smooth-dolt {}: {}", args.join(" "), self.bin.display()))?;

        let what = format!("smooth-dolt {}", args.first().unwrap_or(&""));
        let output = wait_child_draining(child, Some(timeout), &what, SYNC_STALL_HINT)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_clip: String = stderr.trim().chars().take(300).collect();
            anyhow::bail!(
                "smooth-dolt {} failed (exit {}): {}",
                args.first().unwrap_or(&""),
                output.status.code().unwrap_or(-1),
                if stderr_clip.is_empty() { "(no stderr)" } else { stderr_clip.as_str() }
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Wait for a spawned child (stdout/stderr piped) up to an optional
/// deadline, draining both pipes on background threads the whole time.
///
/// Without the drain, a child that writes more than the ~64KB pipe
/// buffer blocks on write and looks "stalled" — it then gets killed at
/// the deadline even though the transfer was healthy. Pearl th-6c6843:
/// `th pearls doctor`'s remote clone of a 2547-commit store died this
/// way at ANY timeout, while quiet push/pull only survived by writing
/// little output.
///
/// On deadline the child is SIGKILLed (releases the noms LOCK), reaped,
/// and a **retryable** error is returned — the message keeps the
/// "timed out after" + "remote sync stalled" markers that
/// [`is_sync_timeout_err`] matches.
///
/// `stall_hint` is appended to the timeout error. Remote-sync callers
/// pass [`SYNC_STALL_HINT`] — [`is_sync_timeout_err`] keys off it to
/// classify the failure as retryable — while local one-shots pass their
/// own, so a stalled local query is never mistaken for a network stall.
fn wait_child_draining(mut child: std::process::Child, timeout: Option<Duration>, what: &str, stall_hint: &str) -> Result<std::process::Output> {
    fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    }
    let out_thread = drain(child.stdout.take());
    let err_thread = drain(child.stderr.take());

    let status = if let Some(timeout) = timeout {
        // Poll for completion up to `timeout`. A short poll interval keeps
        // latency low for fast syncs while not busy-spinning.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().with_context(|| format!("poll {what}"))? {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                // Do NOT join the drain threads here: a grandchild that
                // survives the SIGKILL (e.g. `sh -c` that forked instead of
                // exec'ing) keeps the pipe write-end open, and read_to_end
                // blocks until it dies — joining would hold this abort
                // hostage for the grandchild's lifetime (CI caught exactly
                // that: the 1s-bound stall test took the full 30s). Drop the
                // handles instead; the detached threads exit on pipe EOF.
                drop(out_thread);
                drop(err_thread);
                anyhow::bail!("{what} timed out after {}s ({stall_hint})", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    } else {
        child.wait().with_context(|| format!("wait {what}"))?
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    Ok(std::process::Output { status, stdout, stderr })
}

/// Heuristic: a remote sync (`push`/`pull`) that we aborted on the
/// wallclock bound.
///
/// Distinct from a transport or lock-wedge error because the remediation
/// is "retry the sync later" — the local store is healthy (we killed the
/// child precisely to keep it that way), so callers can treat the local
/// write as durable and the sync as best-effort / deferrable rather than
/// fatal.
pub fn is_sync_timeout_err(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("timed out after") && s.contains("remote sync stalled")
}

#[cfg(test)]
mod shared_git_cache_tests {
    use super::{link_shared_git_cache, shared_cache_key_from_url};
    use std::fs;

    #[test]
    fn key_from_common_remote_url_shapes() {
        assert_eq!(
            shared_cache_key_from_url("git+ssh://git@github.com/SmooAI/smooth.git").as_deref(),
            Some("SmooAI_smooth")
        );
        assert_eq!(
            shared_cache_key_from_url("git+ssh://git@github.com/SmooAI/smooai.git").as_deref(),
            Some("SmooAI_smooai")
        );
        assert_eq!(shared_cache_key_from_url("https://github.com/SmooAI/smooth").as_deref(), Some("SmooAI_smooth"));
        // Same repo, trailing slash → same key (all worktrees share one cache).
        assert_eq!(
            shared_cache_key_from_url("git+ssh://git@github.com/SmooAI/smooth.git/"),
            shared_cache_key_from_url("git+ssh://git@github.com/SmooAI/smooth.git")
        );
        // Different repos → different keys.
        assert_ne!(
            shared_cache_key_from_url("...github.com/SmooAI/smooth.git"),
            shared_cache_key_from_url("...github.com/SmooAI/smooai.git")
        );
        // Garbage with no owner/repo tail → no sharing.
        assert_eq!(shared_cache_key_from_url("nonsense"), None);
    }

    /// Build a fake initialized store with an `origin` remote and return its data dir.
    fn fake_store(root: &std::path::Path, url: &str) -> std::path::PathBuf {
        let data = root.join("wt").join(".smooth").join("dolt").join("pearls");
        let dot_dolt = data.join(".dolt");
        fs::create_dir_all(&dot_dolt).unwrap();
        fs::write(dot_dolt.join("repo_state.json"), format!(r#"{{"remotes":{{"origin":{{"url":"{url}"}}}}}}"#)).unwrap();
        data
    }

    #[cfg(unix)]
    #[test]
    fn links_fresh_worktree_to_shared_and_migrates_warm_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let shared_root = tmp.path().join("shared");
        let data = fake_store(tmp.path(), "git+ssh://git@github.com/SmooAI/smooth.git");
        let local = data.join(".dolt").join("git-remote-cache");

        // (1) No local cache yet → creates a symlink into the shared slot.
        link_shared_git_cache(&data, &shared_root);
        assert!(fs::symlink_metadata(&local).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&local).unwrap(), shared_root.join("SmooAI_smooth"));

        // (2) Idempotent: a second call is a no-op, link unchanged.
        link_shared_git_cache(&data, &shared_root);
        assert_eq!(fs::read_link(&local).unwrap(), shared_root.join("SmooAI_smooth"));

        // (3) A warm real-dir cache migrates its contents into the shared slot.
        let data2 = fake_store(&tmp.path().join("second"), "git+ssh://git@github.com/SmooAI/smooth.git");
        let local2 = data2.join(".dolt").join("git-remote-cache");
        fs::remove_dir_all(&shared_root).ok(); // start clean so migration path is exercised
        fs::create_dir_all(&local2).unwrap();
        fs::write(local2.join("warm.pack"), b"cached").unwrap();
        link_shared_git_cache(&data2, &shared_root);
        assert!(fs::symlink_metadata(&local2).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(shared_root.join("SmooAI_smooth").join("warm.pack")).unwrap(), b"cached");
    }
}

#[cfg(test)]
mod sync_timeout_tests {
    use super::{is_sync_timeout_err, parse_sync_timeout};
    use std::time::Duration;

    #[test]
    fn parse_default_when_unset_via_caller() {
        // The env-read wrapper falls back to the default; here we test the
        // pure parser's branches directly.
        assert_eq!(parse_sync_timeout("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_sync_timeout("  5 "), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_zero_disables_bound() {
        assert_eq!(parse_sync_timeout("0"), None);
    }

    #[test]
    fn parse_garbage_falls_back_to_default_not_unbounded() {
        // Fail-safe: a typo must not silently drop the protection. Falls
        // back to the 300s default bound, not unbounded (pearl th-e814a9).
        assert_eq!(parse_sync_timeout("banana"), Some(Duration::from_secs(300)));
        assert_eq!(parse_sync_timeout("-5"), Some(Duration::from_secs(300)));
        assert_eq!(parse_sync_timeout(""), Some(Duration::from_secs(300)));
    }

    #[test]
    fn classifies_sync_timeout_error() {
        let e = anyhow::anyhow!("smooth-dolt push timed out after 30s (remote sync stalled; killed child to release lock — retryable)");
        assert!(is_sync_timeout_err(&e));
    }

    #[test]
    fn does_not_classify_unrelated_timeouts() {
        // A generic transport "timed out" is NOT a sync-timeout — it
        // lacks the "remote sync stalled" marker.
        assert!(!is_sync_timeout_err(&anyhow::anyhow!("read response: timed out")));
        assert!(!is_sync_timeout_err(&anyhow::anyhow!("syntax error")));
    }
}

// Unix-only: every test here drives a real `/bin/sh` child with POSIX shell
// syntax (`$((i+1))`, `&` backgrounding) to exercise the timeout/kill paths.
// Windows has no `/bin/sh`, so the spawn fails before the behaviour under test
// is reached — and the two tests that assert an *error* would then pass for
// entirely the wrong reason (pearl th-a165b4). `run_cli_timed` itself is
// cross-platform; it is this harness that is POSIX-bound.
#[cfg(all(test, unix))]
mod run_cli_timed_tests {
    use super::SmoothDolt;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Build a SmoothDolt whose "binary" is `/bin/sh` so we can drive
    /// arbitrary child behavior through the args. `data_dir` is unused by
    /// these tests.
    fn sh_handle() -> SmoothDolt {
        SmoothDolt::with_bin(PathBuf::from("/bin/sh"), PathBuf::from("/tmp/unused"))
    }

    #[test]
    fn fast_command_completes_within_bound() {
        let h = sh_handle();
        // `sh -c 'printf hi'` exits immediately with stdout "hi".
        let out = h
            .run_cli_timed(&["-c", "printf hi"], Some(Duration::from_secs(5)))
            .expect("fast command should succeed");
        assert_eq!(out, "hi");
    }

    #[test]
    fn chatty_command_is_not_mistaken_for_stalled() {
        // Regression for pearl th-6c6843: a child writing more than the
        // ~64KB pipe buffer blocked on write (nobody drained until after
        // exit), looked stalled, and was killed at the deadline. 400KB of
        // output finishing instantly must succeed, not time out.
        let h = sh_handle();
        let out = h
            .run_cli_timed(
                &["-c", "i=0; while [ $i -lt 5000 ]; do printf '%080d\\n' $i; i=$((i+1)); done"],
                Some(Duration::from_secs(10)),
            )
            .expect("chatty-but-fast command must not be killed as stalled");
        assert_eq!(out.lines().count(), 5000);
    }

    #[test]
    fn stalled_command_is_aborted_and_returns_retryable_error() {
        let h = sh_handle();
        let start = Instant::now();
        // `sh -c 'sleep 30'` blocks far past the 1s bound — simulates a
        // hung git transfer holding the lock.
        let err = h
            .run_cli_timed(&["-c", "sleep 30"], Some(Duration::from_secs(1)))
            .expect_err("stalled command must time out");
        let elapsed = start.elapsed();
        // We should have aborted at ~1s, nowhere near the 30s sleep.
        assert!(elapsed < Duration::from_secs(5), "aborted promptly, elapsed={elapsed:?}");
        assert!(super::is_sync_timeout_err(&err), "error must be classified retryable: {err:#}");
    }

    #[test]
    fn kill_is_not_blocked_by_grandchild_holding_pipe() {
        // th-6c6843 CI regression: the backgrounded `sleep` survives the
        // SIGKILL of `sh` and keeps the stdout pipe write-end open. The
        // abort must not join the drain threads (read_to_end would block
        // on that open pipe for the grandchild's lifetime).
        let h = sh_handle();
        let start = Instant::now();
        let err = h
            .run_cli_timed(&["-c", "sleep 30 & sleep 30"], Some(Duration::from_secs(1)))
            .expect_err("must time out");
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(5), "abort must not wait for the grandchild, elapsed={elapsed:?}");
        assert!(super::is_sync_timeout_err(&err), "error must be classified retryable: {err:#}");
    }

    #[test]
    fn none_timeout_runs_unbounded_path() {
        // With no bound, a fast command still completes normally (we don't
        // exercise the unbounded-hang case for obvious reasons).
        let h = sh_handle();
        let out = h.run_cli_timed(&["-c", "printf ok"], None).expect("unbounded fast command");
        assert_eq!(out, "ok");
    }
}

/// Locate the smooth-dolt-launcher binary — a tiny C wrapper that
/// resets the signal mask, closes inherited fds, and `setsid`s
/// before exec'ing the real program. Used to spawn `smooth-dolt
/// serve` from inside long-running Tokio processes (Big Smooth)
/// without contaminating Go's runtime with parent state. See
/// `c/smooth-dolt-launcher/launcher.c` for the rationale.
///
/// Resolution mirrors `find_smooth_dolt_binary` but looks for
/// `smooth-dolt-launcher` instead. Returns `None` when not found
/// (callers should fall back to a direct spawn — works fine for
/// short-lived parents like `th` CLI).
pub fn find_smooth_dolt_launcher_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_DOLT_LAUNCHER") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut dir = PathBuf::from(manifest);
        for _ in 0..5 {
            let candidate = dir.join("target").join("release").join("smooth-dolt-launcher");
            if candidate.is_file() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("smooth-dolt-launcher");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let output = Command::new("which").arg("smooth-dolt-launcher").output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Locate the smooth-dolt binary.
///
/// Resolution order:
///  1. `SMOOTH_DOLT` env var (absolute path)
///  2. `target/release/smooth-dolt` relative to CARGO_MANIFEST_DIR (dev builds)
///  3. Same directory as the current executable (installed alongside `th`)
///  4. `PATH` lookup
pub fn find_smooth_dolt_binary() -> Option<PathBuf> {
    // 1. Explicit env var.
    if let Ok(p) = std::env::var("SMOOTH_DOLT") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    // 2. Workspace target/ directory (dev).
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut dir = PathBuf::from(manifest);
        for _ in 0..5 {
            let candidate = dir.join("target").join("release").join("smooth-dolt");
            if candidate.is_file() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // 3. Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("smooth-dolt");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 4. PATH lookup.
    which_smooth_dolt()
}

/// Heuristic: treat broken-pipe / EOF / timeout / closed-connection
/// errors as transport-layer failures eligible for one round of
/// self-heal retry. Errors from the SQL engine itself (syntax, lock,
/// not-found) are NOT transport — those should propagate so callers
/// can react meaningfully instead of looping into an infinite respawn.
fn is_transport_err(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    [
        "broken pipe",
        "connection refused",
        "connection reset",
        "connection closed",
        "server closed connection",
        "timed out",
        "timeout",
        "early eof",
        "unexpected end of file",
        "no such file or directory",
        "transport endpoint",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

#[cfg(test)]
mod is_transport_err_tests {
    use super::is_transport_err;

    #[test]
    fn flags_pipe_and_timeout() {
        assert!(is_transport_err(&anyhow::anyhow!("write request: broken pipe")));
        assert!(is_transport_err(&anyhow::anyhow!("read response: timed out")));
        assert!(is_transport_err(&anyhow::anyhow!("smooth-dolt server closed connection")));
        assert!(is_transport_err(&anyhow::anyhow!("connect /tmp/foo: No such file or directory")));
    }

    #[test]
    fn does_not_flag_sql_errors() {
        assert!(!is_transport_err(&anyhow::anyhow!("smooth-dolt: dolt_add: Error 1105: cannot update manifest")));
        assert!(!is_transport_err(&anyhow::anyhow!("syntax error near 'SELET'")));
    }
}

/// Heuristic for "smooth-dolt server is alive but the dolt engine
/// is wedged in read-only mode" — Pearl th-a97d1f. Triggered by
/// stale LOCK files / interrupted writers leaving the on-disk
/// state with no writable session, even though the serve goroutine
/// answers ping. Force-respawning the child unstuck this case in
/// real-world reproductions today; killing PID and letting the
/// daemon respawn cleared the wedge.
///
/// Narrow on purpose: only the specific shapes Dolt produces for
/// this failure mode. Other Error 1105 / lock errors (deliberate
/// rejection from the user's intent) should propagate.
fn is_lock_wedge_err(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    [
        // Dolt's exact wording when the manifest goroutine has lost
        // its writable session — caught in iter 22 of the bench loop.
        "cannot update manifest: database is read only",
        "cannot update manifest: read-only",
        // Older Dolt builds vary slightly on phrasing.
        "manifest is read-only",
        "cannot acquire write lock",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

/// First backoff delay after a read-only lock-flap. Kept small — most
/// flaps clear within one or two hundred ms (the peer's push commit).
const LOCK_FLAP_BASE_DELAY: Duration = Duration::from_millis(50);

/// Per-attempt backoff ceiling. Doubling from the base tops out here so a
/// long-running peer push doesn't stretch individual waits past ~1.5s.
const LOCK_FLAP_MAX_DELAY: Duration = Duration::from_millis(1500);

/// Total wallclock budget for waiting out a read-only flap before giving
/// up with a clear error. Generous enough to outlast a normal incremental
/// peer push (~10s) with headroom; a genuinely stuck store surfaces the
/// error (and the `th pearls doctor --reap` hint) rather than hanging
/// forever. Override with `SMOOTH_DOLT_LOCK_RETRY_BUDGET_SECS`.
const DEFAULT_LOCK_FLAP_BUDGET_SECS: u64 = 30;

fn lock_flap_budget() -> Duration {
    std::env::var("SMOOTH_DOLT_LOCK_RETRY_BUDGET_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(Duration::from_secs(DEFAULT_LOCK_FLAP_BUDGET_SECS), Duration::from_secs)
}

/// Jitter a backoff delay to `[0.5, 1.5) × delay`, decorrelating
/// concurrent retriers so N agents that flapped at the same instant don't
/// all wake and re-collide in lockstep. Seeded from wall-clock subsec
/// nanos — no `rand` dependency needed; jitter quality is ample here.
fn jittered(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let frac = f64::from(nanos) / 1_000_000_000.0; // [0, 1)
    delay.mul_f64(0.5 + frac)
}

/// The single place the transient "database is read only" lock-flap is
/// handled. Every dolt write funnels through here (CLI `run_cli` /
/// `run_cli_timed` and server `run_with_self_heal`), replacing the two
/// former one-shot ad-hoc retries.
///
/// The flap has two causes and this covers both:
///   1. A **stuck local holder** — an orphaned `smooth-dolt serve` or a
///      wedged in-process child pinning the noms LOCK. `recover` runs
///      ONCE on the first flap to clear it (CLI: reap the orphan; server:
///      force-respawn), after which the retry succeeds.
///   2. A **live peer** — another agent's push briefly holding the
///      single-writer lock. `recover` finds nothing to clear, so we back
///      off with jitter and retry until the peer's commit lands.
///
/// Non-lock errors (syntax, transport, corruption) propagate immediately.
/// Bounded by [`lock_flap_budget`]; on exhaustion the read-only error is
/// returned with a doctor hint rather than hanging forever.
fn retry_on_lock_flap<T>(mut op: impl FnMut() -> Result<T>, mut recover: impl FnMut()) -> Result<T> {
    let deadline = std::time::Instant::now() + lock_flap_budget();
    let mut delay = LOCK_FLAP_BASE_DELAY;
    let mut recovered = false;
    let mut attempts = 0u32;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if is_lock_wedge_err(&e) => {
                attempts += 1;
                if !recovered {
                    recover();
                    recovered = true;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(e.context(format!(
                        "dolt store stayed read-only after {attempts} attempt(s) over {:?}; another writer may be stuck — try `th pearls doctor --reap`",
                        lock_flap_budget()
                    )));
                }
                std::thread::sleep(jittered(delay));
                delay = std::cmp::min(delay.saturating_mul(2), LOCK_FLAP_MAX_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Decision for a single process found holding the noms LOCK. The
/// classification is a PURE function of the holder's own command line
/// and (when relevant) its parent's command line, so it's exhaustively
/// unit-testable without spawning real processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockHolderAction {
    /// Holder is an orphaned `smooth-dolt serve` — clear it (original
    /// Pearl th-49e37b behavior).
    ClearOrphanServer,
    /// Holder is a `git` (or `git-remote-*`) process whose PARENT is
    /// `smooth-dolt` — a stalled dolt-sync child pinning the LOCK during
    /// a hung remote push/pull. Clear it (Pearl: dolt-sync-timeout-
    /// selfheal). This is the recovery safety net for a sync that
    /// stalled before the wallclock timeout could kill it (e.g. the
    /// sync ran in server mode, or in an older build without the bound).
    ClearStalledSyncChild,
    /// Anything else — a debugger, a backup tool, an unrelated git, an
    /// editor. NEVER kill: refuse and propagate the original error.
    Refuse,
}

/// Pure classifier: given the holder's command line and its parent's
/// command line (both as raw `ps -o command=` output, may be empty if
/// the lookup failed), decide what to do.
///
/// Safety invariant — the ONLY things we ever clear:
///   1. `smooth-dolt serve` (the holder itself), or
///   2. a `git` holder whose PARENT command line names `smooth-dolt`.
///
/// A `git` whose parent is NOT smooth-dolt (e.g. the user's own
/// `git push` in a shell, or a git invoked by an IDE) is REFUSED — we
/// must not reach outside this store's own sync machinery. An unrelated
/// non-git, non-serve holder is likewise refused.
fn classify_lock_holder(holder_cmd: &str, parent_cmd: &str) -> LockHolderAction {
    let holder = holder_cmd.to_lowercase();
    let parent = parent_cmd.to_lowercase();

    // Case 1: the original orphaned `smooth-dolt serve`.
    if holder.contains("smooth-dolt") && holder.contains("serve") {
        return LockHolderAction::ClearOrphanServer;
    }

    // Case 2: a stalled dolt-sync git child. The holder must be a git
    // process AND its parent must be smooth-dolt. Both conditions are
    // required — this is what keeps us from killing an unrelated git.
    if holder_is_git(&holder) && parent.contains("smooth-dolt") {
        return LockHolderAction::ClearStalledSyncChild;
    }

    LockHolderAction::Refuse
}

/// Does this command line look like a git remote-transfer process?
/// Matches the `git` driver itself and the `git-remote-*` /
/// `git-upload-pack` / `git-receive-pack` helpers Dolt's sync spawns.
fn holder_is_git(cmd_lower: &str) -> bool {
    // Match on a `git` token boundary so we don't false-match e.g.
    // "digital" or a path component. Cheap heuristic: the basename or an
    // arg starts with "git".
    cmd_lower.split_whitespace().any(|tok| {
        let base = tok.rsplit('/').next().unwrap_or(tok);
        base == "git" || base.starts_with("git-remote") || base.starts_with("git-upload") || base.starts_with("git-receive")
    })
}

/// Look up a PID's command line via `ps -p <pid> -o command=`. Returns
/// an empty string on any failure (process gone, ps missing) — callers
/// treat empty as "unknown", which classifies as Refuse (fail safe).
fn ps_command(pid: u32) -> String {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Look up a PID's parent PID via `ps -p <pid> -o ppid=`. Returns `None`
/// on any failure.
fn ps_parent_pid(pid: u32) -> Option<u32> {
    let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "ppid="]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}

/// Auto-doctor — find processes holding the LOCK file under
/// `data_dir/.dolt/noms/LOCK` and clear the ones that are safe to clear.
/// Returns the number of PIDs cleared (0 if none found / none eligible,
/// which is the normal happy-path case where the read-only error came
/// from a different cause and we should propagate it).
///
/// Two clearable shapes (see [`classify_lock_holder`]):
///
/// 1. **Orphaned `smooth-dolt serve`** (Pearl th-49e37b): an earlier
///    `th up` spawned `smooth-dolt serve <dir> --socket <path>`. The
///    parent died but the serve child got reparented to init and the
///    socket file got cleaned up, leaving the serve process holding the
///    noms LOCK with no way to reach it. CLI calls then fall back to CLI
///    mode and hit `Error 1105: cannot update manifest: database is read
///    only`.
///
/// 2. **Stalled dolt-sync git child** (Pearl: dolt-sync-timeout-
///    selfheal): a `smooth-dolt push`/`pull` shelled out to `git` to
///    move `refs/dolt/data`; the network stalled; the git child still
///    holds the noms LOCK. The wallclock timeout normally kills this,
///    but this is the recovery net for paths the timeout doesn't cover
///    (server-mode sync, older builds). We clear it ONLY when the git
///    holder's parent is `smooth-dolt` — never an unrelated git.
///
/// Escalation: SIGTERM, brief wait, SIGKILL if still alive. The OS
/// releases file locks on death; the retry succeeds. Best-effort: any
/// errors in the doctor itself (e.g. `lsof`/`ps` not on PATH) silently
/// return 0 so we fall through to the original read-only error rather
/// than masking a real bug.
fn auto_doctor_clear_orphan_server(data_dir: &Path) -> Result<u32> {
    let lock_path = data_dir.join("pearls").join(".dolt").join("noms").join("LOCK");
    if !lock_path.exists() {
        return Ok(0);
    }

    // `lsof -t <file>` prints holder PIDs, one per line. Exit code is
    // 1 when there are no holders, which we want to treat as "no
    // orphan found, propagate the original error" — NOT as a doctor
    // failure. `-Fp` would let us parse without ambiguity but `-t` is
    // simpler and matches every macOS + Linux lsof since forever.
    let output = Command::new("lsof").args(["-t", lock_path.to_string_lossy().as_ref()]).output();
    let Ok(output) = output else {
        return Ok(0); // lsof not available — best-effort doctor stays silent
    };
    if !output.status.success() && output.status.code() != Some(1) {
        return Ok(0);
    }

    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .collect();

    if pids.is_empty() {
        return Ok(0);
    }

    let mut cleared = 0u32;
    for pid in pids {
        let holder_cmd = ps_command(pid);
        // Only look up the parent when the holder is a git process —
        // saves a `ps` call on the common serve case and makes the
        // intent explicit.
        let parent_cmd = if holder_is_git(&holder_cmd.to_lowercase()) {
            ps_parent_pid(pid).map(ps_command).unwrap_or_default()
        } else {
            String::new()
        };

        match classify_lock_holder(&holder_cmd, &parent_cmd) {
            LockHolderAction::Refuse => {
                tracing::warn!(
                    pid,
                    cmdline = %holder_cmd,
                    "auto_doctor: process holds the dolt LOCK file but is neither an orphaned `smooth-dolt serve` nor a stalled smooth-dolt sync child — refusing to kill"
                );
            }
            LockHolderAction::ClearOrphanServer => {
                tracing::warn!(pid, "auto_doctor: clearing orphaned `smooth-dolt serve` holding noms LOCK");
                kill_with_escalation(pid);
                cleared += 1;
            }
            LockHolderAction::ClearStalledSyncChild => {
                tracing::warn!(
                    pid,
                    cmdline = %holder_cmd,
                    "auto_doctor: clearing stalled smooth-dolt sync child (git) holding noms LOCK"
                );
                kill_with_escalation(pid);
                cleared += 1;
            }
        }
    }

    if cleared > 0 {
        // Give the OS a moment to actually release the locks. Without
        // this the retry races the kernel's fd-cleanup pass and we
        // get a second false read-only error.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    Ok(cleared)
}

/// SIGTERM a PID, briefly wait, then SIGKILL if it's still alive.
/// SIGTERM gives a `smooth-dolt serve` its graceful-shutdown path and a
/// git child a chance to unwind; the SIGKILL fallback handles a process
/// truly wedged on a dead socket (the stalled-sync case) that ignores
/// SIGTERM. Either way the OS releases the file lock when the process
/// finally dies.
fn kill_with_escalation(pid: u32) {
    let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).stderr(Stdio::null()).status();
    // Poll briefly for the process to exit. `kill -0` succeeds while the
    // process is alive (or a zombie we can still signal). Its stderr is
    // discarded: once the process is gone, `kill -0` prints "No such
    // process" — which is the SUCCESS case here, not something to show.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(pid, "auto_doctor: process survived SIGTERM, escalating to SIGKILL");
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).stderr(Stdio::null()).status();
            return;
        }
    }
}

#[cfg(test)]
mod classify_lock_holder_tests {
    use super::{classify_lock_holder, holder_is_git, LockHolderAction};

    #[test]
    fn orphan_serve_is_cleared() {
        assert_eq!(
            classify_lock_holder("/usr/local/bin/smooth-dolt serve /repo/.smooth/dolt --socket /tmp/x.sock", ""),
            LockHolderAction::ClearOrphanServer
        );
    }

    #[test]
    fn git_child_of_smooth_dolt_is_cleared() {
        // The stalled-sync case: a git transfer whose parent is the
        // smooth-dolt push process pinning the LOCK.
        assert_eq!(
            classify_lock_holder(
                "git-remote-https origin https://github.com/SmooAI/smooth.git",
                "smooth-dolt push /repo/.smooth/dolt"
            ),
            LockHolderAction::ClearStalledSyncChild
        );
        assert_eq!(
            classify_lock_holder("/usr/bin/git push origin", "/usr/local/bin/smooth-dolt pull /repo/.smooth/dolt"),
            LockHolderAction::ClearStalledSyncChild
        );
    }

    #[test]
    fn unrelated_git_is_refused() {
        // SAFETY GUARD: a git whose parent is NOT smooth-dolt (the user's
        // own shell, an IDE, a CI runner) must never be touched.
        assert_eq!(classify_lock_holder("git push origin main", "/bin/zsh"), LockHolderAction::Refuse);
        assert_eq!(classify_lock_holder("git fetch", "node /path/to/vscode"), LockHolderAction::Refuse);
        // Even with an empty (unknown) parent — fail safe.
        assert_eq!(classify_lock_holder("git push", ""), LockHolderAction::Refuse);
    }

    #[test]
    fn unrelated_nongit_holder_is_refused() {
        // A debugger / backup tool / editor that happened to open the
        // LOCK file. Never kill.
        assert_eq!(classify_lock_holder("/usr/bin/lldb", "smooth-dolt push"), LockHolderAction::Refuse);
        assert_eq!(classify_lock_holder("vim LOCK", ""), LockHolderAction::Refuse);
        assert_eq!(classify_lock_holder("rsync -a .dolt backup:/", "smooth-dolt"), LockHolderAction::Refuse);
    }

    #[test]
    fn smooth_dolt_non_serve_holder_is_refused() {
        // A `smooth-dolt push` itself (not `serve`, not a git child)
        // holding the lock is its own legitimate writer — don't kill the
        // sync process directly here; the wallclock timeout owns that.
        assert_eq!(classify_lock_holder("smooth-dolt push /repo/.smooth/dolt", ""), LockHolderAction::Refuse);
    }

    #[test]
    fn holder_is_git_token_boundary() {
        assert!(holder_is_git("git push"));
        assert!(holder_is_git("/usr/bin/git fetch"));
        assert!(holder_is_git("git-remote-https origin url"));
        assert!(holder_is_git("git-upload-pack /repo"));
        // Must not false-match substrings.
        assert!(!holder_is_git("digital-ocean-agent"));
        assert!(!holder_is_git("/opt/legit/server"));
        assert!(!holder_is_git("smooth-dolt serve"));
    }
}

#[cfg(test)]
mod auto_doctor_tests {
    use super::auto_doctor_clear_orphan_server;

    #[test]
    fn returns_zero_when_lock_file_missing() {
        // Empty temp dir → no `pearls/.dolt/noms/LOCK` → doctor is a
        // silent no-op. Whatever caused the read-only error wasn't an
        // orphaned server, so we propagate the original error.
        let tmp = tempfile::tempdir().unwrap();
        let cleared = auto_doctor_clear_orphan_server(tmp.path()).unwrap();
        assert_eq!(cleared, 0);
    }

    #[test]
    fn returns_zero_when_lock_file_exists_but_no_holder() {
        // LOCK file present, no process holds it. lsof exits 1 with
        // no output. Doctor treats this as "nothing to do" (cleared =
        // 0) and the caller falls through to the original error.
        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("pearls").join(".dolt").join("noms");
        std::fs::create_dir_all(&lock_dir).unwrap();
        std::fs::write(lock_dir.join("LOCK"), b"").unwrap();
        let cleared = auto_doctor_clear_orphan_server(tmp.path()).unwrap();
        assert_eq!(cleared, 0);
    }

    #[test]
    fn refuses_to_kill_non_smooth_dolt_holder() {
        // We open the LOCK file from the test process and verify the
        // doctor sees us holding it (via lsof) but DOESN'T kill us —
        // the process command check should reject "anything that
        // isn't `smooth-dolt serve`." This is the safety net that
        // prevents the doctor from accidentally killing a debugger,
        // a backup tool, or an IDE that opened the file.
        //
        // Test process command name is `dolt-XXXX` (cargo test
        // binary) — definitely not `smooth-dolt serve`.
        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("pearls").join(".dolt").join("noms");
        std::fs::create_dir_all(&lock_dir).unwrap();
        let lock_path = lock_dir.join("LOCK");
        let _holder = std::fs::File::create(&lock_path).unwrap();
        // Keep the file open for the duration of the call.
        let cleared = auto_doctor_clear_orphan_server(tmp.path()).unwrap();
        assert_eq!(cleared, 0, "doctor must not kill non-smooth-dolt holders");
        // We're still alive (panic-free) — that's the real assertion.
        assert!(lock_path.exists());
    }
}

#[cfg(test)]
mod is_lock_wedge_err_tests {
    use super::is_lock_wedge_err;

    #[test]
    fn flags_canonical_wedge() {
        // Real error from the bench loop today.
        assert!(is_lock_wedge_err(&anyhow::anyhow!(
            "smooth-dolt exec failed (exit 1): smooth-dolt: exec: Error 1105: cannot update manifest: database is read only"
        )));
    }

    #[test]
    fn flags_variant_phrasings() {
        assert!(is_lock_wedge_err(&anyhow::anyhow!("manifest is read-only")));
        assert!(is_lock_wedge_err(&anyhow::anyhow!("cannot acquire write lock on dolt repo")));
    }

    #[test]
    fn does_not_flag_unrelated_errors() {
        assert!(!is_lock_wedge_err(&anyhow::anyhow!("syntax error near 'SELET'")));
        assert!(!is_lock_wedge_err(&anyhow::anyhow!("table 'pearls' doesn't exist")));
        // Plain Error 1105 without the "read only" qualifier should
        // NOT trigger force-respawn — could be a legit user-driven
        // constraint violation.
        assert!(!is_lock_wedge_err(&anyhow::anyhow!("Error 1105: duplicate column name 'id'")));
    }
}

fn which_smooth_dolt() -> Option<PathBuf> {
    let output = Command::new("which").arg("smooth-dolt").output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Check if a `.smooth/dolt/` directory exists in any parent of `start_dir`.
pub fn find_repo_dolt_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".smooth").join("dolt");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod lock_flap_tests {
    use super::{jittered, retry_on_lock_flap, LOCK_FLAP_BASE_DELAY, LOCK_FLAP_MAX_DELAY};
    use std::cell::Cell;
    use std::time::Duration;

    const WEDGE: &str = "smooth-dolt: exec: Error 1105: cannot update manifest: database is read only";

    #[test]
    fn retries_the_flap_then_succeeds_and_recovers_once() {
        let attempts = Cell::new(0u32);
        let recovers = Cell::new(0u32);
        let out: i32 = retry_on_lock_flap(
            || {
                let n = attempts.get();
                attempts.set(n + 1);
                if n < 2 {
                    Err(anyhow::anyhow!(WEDGE)) // flap twice
                } else {
                    Ok(42)
                }
            },
            || recovers.set(recovers.get() + 1),
        )
        .unwrap();
        assert_eq!(out, 42);
        assert_eq!(attempts.get(), 3, "should have retried past the two flaps");
        assert_eq!(recovers.get(), 1, "recover must run exactly once, on the first flap");
    }

    #[test]
    fn non_lock_error_propagates_immediately_without_recover() {
        let attempts = Cell::new(0u32);
        let recovers = Cell::new(0u32);
        let res: anyhow::Result<i32> = retry_on_lock_flap(
            || {
                attempts.set(attempts.get() + 1);
                Err(anyhow::anyhow!("syntax error near 'SELET'"))
            },
            || recovers.set(recovers.get() + 1),
        );
        assert!(res.is_err());
        assert_eq!(attempts.get(), 1, "a non-lock error must not be retried");
        assert_eq!(recovers.get(), 0, "recover must not run for non-lock errors");
    }

    #[test]
    fn success_first_try_never_recovers() {
        let recovers = Cell::new(0u32);
        let out: i32 = retry_on_lock_flap(|| Ok(7), || recovers.set(recovers.get() + 1)).unwrap();
        assert_eq!(out, 7);
        assert_eq!(recovers.get(), 0);
    }

    #[test]
    fn jitter_stays_within_half_to_one_and_a_half() {
        for _ in 0..1000 {
            let j = jittered(Duration::from_millis(100));
            assert!(j >= Duration::from_millis(50) && j < Duration::from_millis(150), "jitter out of band: {j:?}");
        }
    }

    #[test]
    fn backoff_constants_are_sane() {
        assert!(LOCK_FLAP_BASE_DELAY < LOCK_FLAP_MAX_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_binary_resolution_order() {
        // Just verify the function doesn't panic. The binary may or may
        // not exist depending on the dev environment.
        let _ = find_smooth_dolt_binary();
    }

    #[test]
    fn find_repo_dolt_dir_returns_none_for_tmp() {
        let tmp = std::env::temp_dir();
        assert!(find_repo_dolt_dir(&tmp).is_none());
    }
}

/// Classify a Dolt error as on-disk-storage corruption (manifest /
/// chunk index torn write, partial flush across macOS sleep, etc.).
///
/// Treated separately from [`is_transport_err`] and [`is_lock_wedge_err`]
/// because the remediation is different: respawning the server doesn't
/// help — the on-disk state itself needs to be rebuilt. `th pearls
/// doctor` does the rebuild via re-clone from the configured remote.
pub fn is_corruption_err(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    [
        // Canonical wording — produced when noms/manifest is torn or has
        // an invalid leading-version byte.
        "corrupt manifest",
        "current directory is not a valid dolt repository",
        // Chunk index mismatch (manifest references chunks that aren't on disk).
        "chunk not found",
        // Newer Dolt builds occasionally surface this on noms corruption.
        "noms: chunk store",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

#[cfg(test)]
mod is_corruption_err_tests {
    use super::is_corruption_err;

    #[test]
    fn flags_corrupt_manifest() {
        assert!(is_corruption_err(&anyhow::anyhow!("failed to load database with error: corrupt manifest")));
    }

    #[test]
    fn flags_invalid_repo() {
        assert!(is_corruption_err(&anyhow::anyhow!("The current directory is not a valid dolt repository.")));
    }

    #[test]
    fn does_not_flag_unrelated() {
        assert!(!is_corruption_err(&anyhow::anyhow!("syntax error")));
        assert!(!is_corruption_err(&anyhow::anyhow!("cannot update manifest: database is read only")));
    }
}

/// Result of a doctor health check against the on-disk dolt state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorDiagnosis {
    /// Cold CLI probe succeeded — manifest readable, log accessible.
    Healthy,
    /// On-disk noms manifest has unresolved git merge-conflict markers.
    /// Distinct from generic Corrupt because the fix is "pick a side"
    /// (hand-resolve the conflict), not "re-clone from remote".
    ///
    /// Cause: someone (you) git-merged or stashed across branches whose
    /// `.smooth/dolt/*/.dolt/noms/manifest` files diverged, and git's
    /// text-merger turned a single-line binary record into a multi-line
    /// file with `<<<<<<<` / `=======` / `>>>>>>>` markers in it.
    ConflictMarkers {
        /// All non-marker, non-empty candidate lines (raw bytes UTF-8
        /// best-effort) — each is a complete prior-state manifest. The
        /// repair picks the longest (most-data, usually most-recent).
        candidates: Vec<String>,
    },
    /// On-disk storage is corrupt for some other reason. Repair = re-clone
    /// from remote if origin is canonical, or rebuild from chunks.
    Corrupt {
        /// Underlying error message (clipped).
        detail: String,
    },
    /// No dolt dir or unrecognized state. Repair = init or clone.
    NotInitialized { detail: String },
}

/// Best-effort detection of git conflict markers in a noms manifest.
/// Returns Some(candidate_lines) when markers are present, with
/// the candidate lines (i.e. the *content* between markers) ordered
/// by occurrence in the file. Each candidate is a full prior-state
/// manifest line — pick one to recover.
fn detect_manifest_conflict_markers(manifest_path: &std::path::Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(manifest_path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    // Cheap rejection of healthy manifests (single-line, no '<').
    if !text.contains("<<<<<<<") {
        return None;
    }
    let candidates: Vec<String> = text
        .lines()
        .filter(|l| {
            let l = l.trim_end();
            !l.is_empty() && !l.starts_with("<<<<<<<") && !l.starts_with("=======") && !l.starts_with(">>>>>>>") && !l.starts_with("|||||||")
        })
        .map(str::to_string)
        .collect();
    Some(candidates)
}

impl SmoothDolt {
    /// Cold-process probe of the data dir. Uses a CLI handle (never the
    /// attached long-running server) so it actually exercises the noms
    /// manifest read path — the very thing that gets wedged in the
    /// failure mode this guards against.
    ///
    /// Cheap: runs `dolt log -n 1` which loads the manifest + walks one
    /// ref. Returns within ~50–200ms on a healthy dir.
    pub fn diagnose(data_dir: &std::path::Path) -> DoctorDiagnosis {
        // Cheap pre-check: is the manifest itself a git-merge-conflict
        // mess? That's a common cause we can fix without a network
        // round-trip and surfacing it specifically gives the user a
        // much friendlier remediation than "re-clone".
        let manifest = data_dir.join(".dolt").join("noms").join("manifest");
        if let Some(candidates) = detect_manifest_conflict_markers(&manifest) {
            return DoctorDiagnosis::ConflictMarkers { candidates };
        }

        // Missing core pointer files. When `.dolt/` exists but its
        // `noms/manifest` (chunk-store root) or `repo_state.json`
        // (branch/HEAD + remotes) is gone, the store is unreadable —
        // the signature of an interrupted GC/archive or a half-written
        // clone (e.g. SMOODEV pearl store, 2026-06-18). Classify as
        // Corrupt (recoverable by re-clone) rather than letting the
        // cold `log` probe below fall through to NotInitialized, which
        // is a dead-end the doctor/auto-heal won't act on. Pearl
        // th-03cdb8.
        let dolt_meta = data_dir.join(".dolt");
        if dolt_meta.is_dir() {
            let mut missing: Vec<&str> = Vec::new();
            if !manifest.exists() {
                missing.push("noms/manifest");
            }
            if !dolt_meta.join("repo_state.json").exists() {
                missing.push("repo_state.json");
            }
            if !missing.is_empty() {
                return DoctorDiagnosis::Corrupt {
                    detail: format!(
                        "missing core dolt file(s): {} — interrupted GC/archive or half-written clone; re-clone from remote",
                        missing.join(", ")
                    ),
                };
            }
        }

        let cli = match Self::new_cli_only(data_dir) {
            Ok(c) => c,
            Err(e) => {
                return DoctorDiagnosis::NotInitialized {
                    detail: format!("cannot construct CLI handle: {e:#}"),
                };
            }
        };
        match cli.log(1) {
            Ok(_) => DoctorDiagnosis::Healthy,
            Err(e) if is_corruption_err(&e) => DoctorDiagnosis::Corrupt {
                detail: format!("{e:#}").chars().take(400).collect(),
            },
            Err(e) => {
                // Anything else that prevents a cold log probe — we
                // surface as "needs init" with the detail so the user
                // can decide.
                DoctorDiagnosis::NotInitialized {
                    detail: format!("{e:#}").chars().take(400).collect(),
                }
            }
        }
    }

    /// Repair a manifest that has git conflict markers in it. Picks the
    /// longest candidate line (most data, usually the most-recent prior
    /// state) and writes it as the new manifest. Backs up the broken
    /// version to `manifest.with-conflicts-<ts>` so the user can manually
    /// inspect / pick a different line if the longest one isn't right.
    ///
    /// Returns the chosen candidate so the caller can log which one was
    /// picked.
    pub fn repair_manifest_conflict(data_dir: &std::path::Path, candidates: &[String]) -> Result<String> {
        if candidates.is_empty() {
            anyhow::bail!("no candidate manifest lines to choose from");
        }
        let manifest = data_dir.join(".dolt").join("noms").join("manifest");
        // Heuristic: longest line has the most table-entries, almost
        // always the most-recent state. Tied-length → take the last
        // candidate (closer to "their" side of the merge).
        let chosen = candidates
            .iter()
            .enumerate()
            .max_by_key(|(i, c)| (c.len(), *i))
            .map(|(_, c)| c.clone())
            .expect("non-empty");

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = manifest.with_file_name(format!("manifest.with-conflicts-{ts}"));
        std::fs::copy(&manifest, &backup).with_context(|| format!("backup manifest → {}", backup.display()))?;

        // Write without a trailing newline — noms expects a bare record.
        std::fs::write(&manifest, chosen.as_bytes()).with_context(|| format!("write {}", manifest.display()))?;

        Ok(chosen)
    }

    /// Recover from on-disk corruption by snapshotting the broken dir
    /// (so the user can fish unpushed work out of it if needed) and
    /// re-cloning fresh from the configured `origin` remote.
    ///
    /// Returns the path to the snapshotted broken dir on success.
    ///
    /// Caller is responsible for ensuring no `smooth-dolt serve` is
    /// holding a writable handle on `data_dir` — the rename will fail
    /// otherwise. The CLI dispatcher handles this by refusing without
    /// `--force` when a server is attached.
    pub fn recover_from_remote(&self) -> Result<PathBuf> {
        // The clone must target the multi-db ROOT (e.g. `.smooth/dolt`)
        // because `smooth-dolt clone <root>` recreates the `pearls`
        // database at `<root>/pearls`. Callers hand us either the root
        // or the `pearls` repo subdir (the doctor probes per-subdir),
        // so normalize: if we were given the `pearls` repo dir, step up
        // to its parent root before cloning. Pearl th-03cdb8.
        let root: PathBuf = if self.data_dir.file_name().and_then(|n| n.to_str()) == Some("pearls") {
            self.data_dir.parent().context("pearls dir has no parent root")?.to_path_buf()
        } else {
            self.data_dir.clone()
        };
        let pearls_dir = root.join("pearls");

        // Resolve the origin URL. Prefer the dolt repo's own
        // repo_state.json; fall back to the enclosing git repo's
        // `origin` when that file is gone — which is the exact failure
        // mode we recover from, since an interrupted op can wipe
        // repo_state.json. The raw git URL is what `smooth-dolt clone`
        // already consumes.
        let remote_url = read_origin_url(&pearls_dir)
            .or_else(|_| read_git_origin_url(&root))
            .context("no origin remote found (repo_state.json missing AND no git `origin`) — manual `smooth-dolt clone <url> <dir>` required")?;

        let parent = root.parent().context("data_dir has no parent")?;
        let leaf = root.file_name().and_then(|n| n.to_str()).context("data_dir has no leaf name")?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let broken_path = parent.join(format!("{leaf}.broken-{ts}"));
        std::fs::rename(&root, &broken_path).with_context(|| format!("snapshot corrupt dir → {}", broken_path.display()))?;

        match clone_from(&remote_url, &root) {
            Ok(()) => Ok(broken_path),
            Err(e) => {
                // Restore the broken dir so the user isn't stranded.
                let _ = std::fs::rename(&broken_path, &root);
                Err(e).context("re-clone during recovery failed")
            }
        }
    }
}

/// Default minimum age before a hung one-shot `smooth-dolt sql`/`exec`
/// process is considered leaked and safe to reap. One-shots are
/// transient by design (a healthy one lives for tens of milliseconds);
/// anything still alive after this long is wedged, not working. The
/// bound exists purely so a concurrently-launched, legitimately-running
/// one-shot from another `th` isn't killed mid-write.
pub const DEFAULT_REAP_AGE_SECS: u64 = 30;

/// What a running `smooth-dolt` process holding this store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderKind {
    /// `smooth-dolt serve <dir> --socket <path>` — the long-running
    /// server. Legitimately long-lived; only reaped with `--force`.
    Serve,
    /// `smooth-dolt push|pull|fetch|clone <dir>` — a remote sync. THE
    /// wedge: pointed at a malformed remote (the `/./` mangling — see
    /// [`repair_malformed_remote_url`]) git rejects the path and the
    /// push never returns, while holding the noms write lock. Every
    /// other writer of the store then gets `database is read only`, and
    /// their queries pile up BEHIND the lock looking like the cause.
    Sync,
    /// `smooth-dolt sql|exec|log|... <dir> -q …` — a one-shot. Should
    /// live milliseconds; a long-lived one is leaked and pins the store
    /// the same way (usually as a symptom, queued behind a hung Sync).
    OneShot,
    /// A child of one of the above — e.g. the `git fetch` a hung
    /// `smooth-dolt push` spawned. It inherits the lock, so killing only
    /// the parent leaves the store wedged.
    Child,
}

/// A live `smooth-dolt` process whose argv references this project's
/// dolt store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreHolder {
    pub pid: u32,
    /// Full command line as reported by `ps`.
    pub cmd: String,
    /// Wallclock age in seconds (from `ps -o etime=`).
    pub age_secs: u64,
    pub kind: HolderKind,
}

/// Pure predicate: does this `ps` command line belong to a `smooth-dolt`
/// process operating on `dolt_root` (or a database directory beneath
/// it)?
///
/// SAFETY INVARIANT — both halves are required:
///   1. the **executable** (argv[0]) has basename exactly `smooth-dolt`,
///      and
///   2. some argv token is `dolt_root` itself or a path *under* it.
///
/// So a smooth-dolt serving a DIFFERENT project's store never matches,
/// and an unrelated process that merely mentions the binary or the path
/// (an editor, a `grep`, a backup job) never matches either — it isn't
/// argv[0]. Path containment is checked on component boundaries, so
/// `/x/.smooth/dolt-backup` is not treated as being under
/// `/x/.smooth/dolt`.
#[must_use]
pub fn classify_store_process(cmd: &str, dolt_root: &Path) -> Option<HolderKind> {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();

    let exe = Path::new(tokens.first()?);
    if exe.file_name()? != "smooth-dolt" {
        return None;
    }
    if !tokens.iter().any(|tok| path_is_within(Path::new(tok), dolt_root)) {
        return None;
    }

    let sub = tokens.get(1).copied().unwrap_or_default();
    match sub {
        "serve" => Some(HolderKind::Serve),
        "push" | "pull" | "fetch" | "clone" => Some(HolderKind::Sync),
        _ => Some(HolderKind::OneShot),
    }
}

/// Is `candidate` `root` itself, or a path beneath it? Component-wise so
/// a shared string prefix (`dolt` vs `dolt-backup`) can't false-match.
///
/// Both sides are canonicalized when they exist on disk: the argv of a
/// running process carries whatever path its caller typed, while the
/// doctor's root comes from a canonicalized walk-up — on macOS that's
/// the difference between `/var/…` and `/private/var/…`, and comparing
/// them raw finds zero holders for a store that is very much held.
/// Paths that don't exist (unit tests, a since-deleted dir) compare raw.
fn path_is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = candidate.canonicalize().unwrap_or_else(|_| candidate.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    candidate == root || candidate.starts_with(&root)
}

/// Pure decision: should this holder be reaped?
///
/// - A one-shot older than `min_age_secs` is leaked — reap it. It is
///   transient by design; nothing legitimate lives that long.
/// - A `serve` is a legitimate long-running process — only `--force`
///   reaps it (same semantics doctor already applies to an attached
///   server before a re-clone).
/// - A hung `push`/`pull` (and the `git` child it spawned) is the store
///   wedge itself — reap it on the same age rule. A healthy sync is
///   bounded by [`sync_timeout`] and kills itself; one still alive past
///   the threshold is stuck on a remote it can never reach.
#[must_use]
pub fn should_reap(kind: HolderKind, age_secs: u64, min_age_secs: u64, force: bool) -> bool {
    match kind {
        HolderKind::Serve => force,
        HolderKind::Sync | HolderKind::OneShot | HolderKind::Child => force || age_secs >= min_age_secs,
    }
}

/// Enumerate live `smooth-dolt` processes holding this project's store.
///
/// Uses `ps -Ao pid=,etime=,command=` (portable across macOS + Linux)
/// rather than `lsof` on the noms LOCK, because a leaked one-shot is
/// identified by *what store it was pointed at*, not by whether it
/// currently has the LOCK fd open. Never returns our own pid.
///
/// Children of a matched holder are included too: a hung
/// `smooth-dolt push` spawns `git fetch …`, and that child inherits the
/// lock — killing only the parent leaves the store wedged.
///
/// Best-effort: if `ps` is unavailable the result is empty, and the
/// caller falls through to reporting whatever the write probe said.
#[must_use]
pub fn find_store_holders(dolt_root: &Path) -> Vec<StoreHolder> {
    let Ok(output) = Command::new("ps").args(["-Ao", "pid=,ppid=,etime=,command="]).output() else {
        return Vec::new();
    };
    let me = std::process::id();
    let procs: Vec<(u32, u32, u64, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .filter(|(pid, ..)| *pid != me)
        .collect();

    let mut holders: Vec<StoreHolder> = procs
        .iter()
        .filter_map(|(pid, _, age_secs, cmd)| {
            classify_store_process(cmd, dolt_root).map(|kind| StoreHolder {
                pid: *pid,
                cmd: cmd.clone(),
                age_secs: *age_secs,
                kind,
            })
        })
        .collect();

    // Second pass: adopt the children of what we matched. One level is
    // enough — dolt's sync spawns `git` directly.
    // ponytail: one level, not a full tree walk. Deepen if a grandchild
    // ever turns up holding the lock.
    let parents: Vec<u32> = holders.iter().map(|h| h.pid).collect();
    holders.extend(
        procs
            .iter()
            .filter(|(pid, ppid, ..)| parents.contains(ppid) && !parents.contains(pid))
            .map(|(pid, _, age_secs, cmd)| StoreHolder {
                pid: *pid,
                cmd: cmd.clone(),
                age_secs: *age_secs,
                kind: HolderKind::Child,
            }),
    );
    holders
}

/// Parse one `ps -Ao pid=,ppid=,etime=,command=` line into
/// (pid, ppid, age_secs, command).
fn parse_ps_line(line: &str) -> Option<(u32, u32, u64, String)> {
    // `split_whitespace` (not `splitn`) — ps right-pads the numeric
    // columns, so the separators are runs of spaces, not single ones.
    let mut parts = line.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let ppid: u32 = parts.next()?.parse().ok()?;
    let age = parse_etime(parts.next()?)?;
    let cmd = parts.collect::<Vec<_>>().join(" ");
    if cmd.is_empty() {
        return None;
    }
    Some((pid, ppid, age, cmd))
}

/// Parse `ps` elapsed-time (`[[dd-]hh:]mm:ss`) into seconds.
fn parse_etime(raw: &str) -> Option<u64> {
    let (days, hms) = match raw.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, raw),
    };
    let fields: Vec<u64> = hms.split(':').map(|f| f.parse::<u64>().ok()).collect::<Option<_>>()?;
    let (h, m, s) = match fields.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + s)
}

/// Reap the leaked `smooth-dolt` processes holding this store. Returns
/// (reaped, refused) — refused holders are the ones `should_reap` said
/// to leave alone (a `serve` without `--force`, a one-shot too young to
/// call leaked), so the caller can explain itself.
///
/// SIGTERM → brief grace → SIGKILL, via [`kill_with_escalation`].
pub fn reap_store_holders(dolt_root: &Path, min_age_secs: u64, force: bool) -> (Vec<StoreHolder>, Vec<StoreHolder>) {
    let (to_reap, refused): (Vec<_>, Vec<_>) = find_store_holders(dolt_root)
        .into_iter()
        .partition(|h| should_reap(h.kind, h.age_secs, min_age_secs, force));

    for holder in &to_reap {
        tracing::warn!(pid = holder.pid, cmdline = %holder.cmd, "reaping leaked smooth-dolt process holding the pearl store");
        kill_with_escalation(holder.pid);
    }
    if !to_reap.is_empty() {
        // Let the kernel finish releasing the store's file locks before
        // the caller re-probes; otherwise the re-probe races fd cleanup
        // and reports a false read-only.
        std::thread::sleep(Duration::from_millis(500));
    }
    (to_reap, refused)
}

/// Result of the doctor's write-ability probe. [`DoctorDiagnosis`]
/// only tells you whether the store READS — a store pinned by a leaked
/// process reads perfectly and still refuses every write, which is
/// exactly the failure that made doctor say `✓ healthy` while
/// `th pearls create` died (pearl th-118847).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteProbe {
    /// A trivial write succeeded — the store accepts writes.
    Writable,
    /// The store is read-only: writes fail with the Dolt
    /// `cannot update manifest: database is read only` family of errors.
    ReadOnly { detail: String },
    /// The probe itself couldn't run (binary missing, unrelated SQL
    /// failure). Reported, but not treated as a write-lock.
    Failed { detail: String },
}

/// Probe whether `db_dir` actually accepts writes, by creating and
/// dropping a throwaway table. A read-only store fails the CREATE with
/// the manifest error; a healthy store round-trips and is left exactly
/// as it was found.
///
/// Deliberately bypasses the CLI self-heal ([`SmoothDolt::run_cli`]'s
/// auto-doctor retry) — a diagnostic must report what it sees, not
/// silently kill processes as a side effect of looking.
#[must_use]
pub fn probe_writable(db_dir: &Path) -> WriteProbe {
    // Unique per probe: a leftover table from an interrupted earlier
    // probe would turn `CREATE TABLE IF NOT EXISTS` into a no-op that
    // succeeds even on a read-only store — a false "writable".
    let probe_table = format!("_th_doctor_write_probe_{}", std::process::id());

    let cli = match SmoothDolt::new_cli_only(db_dir) {
        Ok(c) => c,
        Err(e) => {
            return WriteProbe::Failed {
                detail: format!("{e:#}").chars().take(200).collect(),
            };
        }
    };

    match cli.exec_no_heal(&format!("CREATE TABLE IF NOT EXISTS {probe_table} (id INT PRIMARY KEY)")) {
        Ok(_) => {
            // Leave no trace. A failure to drop is worth surfacing but
            // isn't a write-lock — writes plainly work.
            if let Err(e) = cli.exec_no_heal(&format!("DROP TABLE IF EXISTS {probe_table}")) {
                tracing::warn!(error = %e, table = probe_table, "doctor write probe could not drop its probe table");
            }
            WriteProbe::Writable
        }
        Err(e) if is_lock_wedge_err(&e) => WriteProbe::ReadOnly {
            detail: format!("{e:#}").chars().take(200).collect(),
        },
        Err(e) => WriteProbe::Failed {
            detail: format!("{e:#}").chars().take(200).collect(),
        },
    }
}

/// The one repair the doctor should attempt for a given state. Exists as
/// a pure function so the invariant that matters can be unit-tested:
/// **a healthy-but-write-locked store gets REAPED, never re-cloned.**
/// Losing a healthy local pearl DB to a re-clone from a broken remote
/// must be impossible (pearl th-118847).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// Nothing to do.
    None,
    /// Kill the leaked processes pinning the store, then re-probe.
    Reap,
    /// Hand-resolve the conflicted noms manifest.
    RepairManifest,
    /// Snapshot + re-clone from `origin`. ONLY when the manifest does
    /// not read cleanly — i.e. the store is genuinely corrupt.
    RecloneFromRemote,
}

#[must_use]
pub fn select_remedy(diagnosis: &DoctorDiagnosis, write_locked: bool) -> Remedy {
    match diagnosis {
        DoctorDiagnosis::ConflictMarkers { .. } => Remedy::RepairManifest,
        DoctorDiagnosis::Corrupt { .. } => Remedy::RecloneFromRemote,
        DoctorDiagnosis::NotInitialized { .. } => Remedy::None,
        DoctorDiagnosis::Healthy if write_locked => Remedy::Reap,
        DoctorDiagnosis::Healthy => Remedy::None,
    }
}

#[cfg(test)]
mod holder_tests {
    use super::{classify_store_process, parse_etime, parse_ps_line, select_remedy, should_reap, DoctorDiagnosis, HolderKind, Remedy, DEFAULT_REAP_AGE_SECS};
    use std::path::Path;

    fn root() -> &'static Path {
        Path::new("/Users/b/dev/smooai/smooai/.smooth/dolt")
    }

    #[test]
    fn matches_hung_push_for_this_store() {
        // THE wedge (pearl th-118847 / th-3ef2c1): a push against a
        // malformed remote that never returns, holding the write lock.
        assert_eq!(
            classify_store_process("smooth-dolt push /Users/b/dev/smooai/smooai/.smooth/dolt", root()),
            Some(HolderKind::Sync)
        );
        assert_eq!(
            classify_store_process("smooth-dolt pull /Users/b/dev/smooai/smooai/.smooth/dolt", root()),
            Some(HolderKind::Sync)
        );
    }

    #[test]
    fn hung_sync_is_reaped_on_the_age_rule() {
        assert!(should_reap(HolderKind::Sync, 45, DEFAULT_REAP_AGE_SECS, false));
        assert!(should_reap(HolderKind::Child, 45, DEFAULT_REAP_AGE_SECS, false));
        // A sync that just started may be a legitimate push in flight.
        assert!(!should_reap(HolderKind::Sync, 2, DEFAULT_REAP_AGE_SECS, false));
    }

    #[test]
    fn matches_one_shot_sql_for_this_store() {
        assert_eq!(
            classify_store_process(
                "/Users/b/.cargo/bin/smooth-dolt sql /Users/b/dev/smooai/smooai/.smooth/dolt -q SELECT * FROM pearl_labels",
                root()
            ),
            Some(HolderKind::OneShot)
        );
    }

    #[test]
    fn matches_serve_for_this_store() {
        assert_eq!(
            classify_store_process("smooth-dolt serve /Users/b/dev/smooai/smooai/.smooth/dolt --socket /tmp/x.sock", root()),
            Some(HolderKind::Serve)
        );
    }

    #[test]
    fn matches_db_subdir_under_the_root() {
        // The doctor probes per-db (`<root>/pearls`); a process pointed
        // at the db dir is still holding this project's store.
        assert_eq!(
            classify_store_process("smooth-dolt exec /Users/b/dev/smooai/smooai/.smooth/dolt/pearls -q INSERT INTO x", root()),
            Some(HolderKind::OneShot)
        );
    }

    #[test]
    fn never_matches_another_projects_store() {
        // SAFETY: the whole point. A smooth-dolt for a different repo,
        // or the global ~/.smooth store, must be invisible to us.
        assert_eq!(
            classify_store_process("smooth-dolt sql /Users/b/dev/smooai/smooth/.smooth/dolt -q SELECT 1", root()),
            None
        );
        assert_eq!(
            classify_store_process("smooth-dolt serve /Users/b/.smooth/dolt --socket /tmp/g.sock", root()),
            None
        );
    }

    #[test]
    fn never_matches_a_sibling_path_with_a_shared_prefix() {
        assert_eq!(
            classify_store_process("smooth-dolt sql /Users/b/dev/smooai/smooai/.smooth/dolt-backup -q SELECT 1", root()),
            None
        );
    }

    #[test]
    fn never_matches_a_non_smooth_dolt_process() {
        // An editor, a grep, a backup job that merely names the path.
        assert_eq!(
            classify_store_process("vim /Users/b/dev/smooai/smooai/.smooth/dolt/pearls/.dolt/noms/LOCK", root()),
            None
        );
        assert_eq!(classify_store_process("rg smooth-dolt /Users/b/dev/smooai/smooai/.smooth/dolt", root()), None);
        assert_eq!(
            classify_store_process("rsync -a /Users/b/dev/smooai/smooai/.smooth/dolt backup:/", root()),
            None
        );
        // `th` itself, which names both the binary and the path.
        assert_eq!(
            classify_store_process("smooth-dolt-wrapper /Users/b/dev/smooai/smooai/.smooth/dolt", root()),
            None
        );
    }

    #[test]
    fn never_matches_smooth_dolt_without_our_path() {
        assert_eq!(classify_store_process("smooth-dolt version", root()), None);
    }

    #[test]
    fn one_shot_is_reaped_only_once_stale() {
        assert!(
            !should_reap(HolderKind::OneShot, 2, DEFAULT_REAP_AGE_SECS, false),
            "a fresh one-shot may be a live query"
        );
        assert!(should_reap(HolderKind::OneShot, 45, DEFAULT_REAP_AGE_SECS, false));
        // --force reaps regardless of age.
        assert!(should_reap(HolderKind::OneShot, 0, DEFAULT_REAP_AGE_SECS, true));
    }

    #[test]
    fn serve_is_only_reaped_with_force() {
        assert!(!should_reap(HolderKind::Serve, 99_999, DEFAULT_REAP_AGE_SECS, false));
        assert!(should_reap(HolderKind::Serve, 1, DEFAULT_REAP_AGE_SECS, true));
    }

    #[test]
    fn healthy_but_write_locked_reaps_and_never_reclones() {
        // THE invariant: a store that reads fine must never be re-cloned
        // from origin just because writes are blocked.
        assert_eq!(select_remedy(&DoctorDiagnosis::Healthy, true), Remedy::Reap);
        assert_eq!(select_remedy(&DoctorDiagnosis::Healthy, false), Remedy::None);
    }

    #[test]
    fn corrupt_manifest_still_reclones() {
        assert_eq!(
            select_remedy(&DoctorDiagnosis::Corrupt { detail: "bad manifest".into() }, false),
            Remedy::RecloneFromRemote
        );
        // Even if it ALSO can't be written, corruption wins — a corrupt
        // store can't be fixed by killing processes.
        assert_eq!(
            select_remedy(&DoctorDiagnosis::Corrupt { detail: "bad manifest".into() }, true),
            Remedy::RecloneFromRemote
        );
        assert_eq!(
            select_remedy(&DoctorDiagnosis::ConflictMarkers { candidates: vec!["x".into()] }, true),
            Remedy::RepairManifest
        );
    }

    #[test]
    fn etime_formats() {
        assert_eq!(parse_etime("05"), None);
        assert_eq!(parse_etime("01:30"), Some(90));
        assert_eq!(parse_etime("02:01:30"), Some(7_290));
        assert_eq!(parse_etime("1-02:01:30"), Some(93_690));
        assert_eq!(parse_etime("garbage"), None);
    }

    #[test]
    fn ps_line_parses() {
        // ps right-pads the numeric columns — the separators are runs of
        // spaces, which is why this can't be a `splitn`.
        assert_eq!(
            parse_ps_line(" 4242   3517 01:05 /usr/local/bin/smooth-dolt sql /x/.smooth/dolt -q SELECT 1"),
            Some((4242, 3517, 65, "/usr/local/bin/smooth-dolt sql /x/.smooth/dolt -q SELECT 1".to_string()))
        );
        assert_eq!(parse_ps_line(""), None);
        assert_eq!(parse_ps_line("notapid 1 01:05 cmd"), None);
    }
}

#[cfg(test)]
mod repair_malformed_remote_url_tests {
    use super::{normalize_remote_url, repair_malformed_remote_url};

    #[test]
    fn repairs_the_real_wedged_remotes() {
        // Both stores on Brent's machine carried these, and both hung
        // every push: git rejects `./SmooAI/smooai` as a repo name.
        assert_eq!(
            repair_malformed_remote_url("git+ssh://git@github.com/./SmooAI/smooai.git").as_deref(),
            Some("git+ssh://git@github.com/SmooAI/smooai.git")
        );
        assert_eq!(
            repair_malformed_remote_url("git+ssh://git@github.com/./brentrager/smooth-home.git").as_deref(),
            Some("git+ssh://git@github.com/brentrager/smooth-home.git")
        );
    }

    #[test]
    fn leaves_a_healthy_remote_alone() {
        assert_eq!(repair_malformed_remote_url("git+ssh://git@github.com/SmooAI/smooth.git"), None);
        assert_eq!(repair_malformed_remote_url("https://github.com/SmooAI/smooth.git"), None);
        assert_eq!(repair_malformed_remote_url("file:///tmp/store"), None);
    }

    #[test]
    fn derivation_never_produces_the_mangling() {
        // The write side (pearl th-c4441b): whatever we hand Dolt must
        // already be a clean URL, so no new store can acquire a `/./`.
        for scp in ["git@github.com:SmooAI/smooai.git", "git@github.com:brentrager/smooth-home.git"] {
            let derived = normalize_remote_url(scp);
            assert!(!derived.contains("/./"), "{scp} derived a mangled URL: {derived}");
            assert_eq!(repair_malformed_remote_url(&derived), None);
        }
        assert_eq!(
            normalize_remote_url("git@github.com:SmooAI/smooai.git"),
            "git+ssh://git@github.com/SmooAI/smooai.git"
        );
    }
}

/// Clone a dolt store from a remote URL into `target_dir`. Used by
/// `th pearls init` for post-`git clone` bootstrap — when a fresh
/// checkout has no `.smooth/dolt/` on disk (it's gitignored under the
/// beads model) but the git remote has `refs/dolt/data` carrying the
/// pearl history.
///
/// Wraps `smooth-dolt clone <remote_url> <target_dir>` with stdin
/// detached so the subprocess can't block waiting on a TTY.
///
/// # Errors
/// - smooth-dolt binary not findable
/// - clone subprocess returns non-zero (network failure, ref not found,
///   etc.) — stderr is captured + first 400 chars included
pub fn clone_from(remote_url: &str, target_dir: &std::path::Path) -> Result<()> {
    clone_from_with_timeout(remote_url, target_dir, None)
}

/// Like [`clone_from`] but bounded by the standard remote-sync timeout
/// ([`sync_timeout`] — 300s default, `SMOOTH_DOLT_SYNC_TIMEOUT_SECS` to
/// override). Used by `th pearls doctor`'s remote-sync probe, where a
/// dead/unreachable remote must produce a diagnosis rather than a hang.
pub fn clone_from_bounded(remote_url: &str, target_dir: &std::path::Path) -> Result<()> {
    clone_from_with_timeout(remote_url, target_dir, sync_timeout())
}

fn clone_from_with_timeout(remote_url: &str, target_dir: &std::path::Path, timeout: Option<Duration>) -> Result<()> {
    let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found for clone — Run: scripts/build-smooth-dolt.sh")?;
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create parent of {}", target_dir.display()))?;
    }
    let remote_url = &normalize_remote_url(remote_url);
    let child = Command::new(&bin)
        .args(["clone", remote_url, &target_dir.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("exec smooth-dolt clone")?;

    // Drain-while-waiting is load-bearing here: a real clone prints enough
    // progress to fill the pipe buffer, which read as "stalled" and got
    // killed at any deadline (pearl th-6c6843).
    let output = wait_child_draining(child, timeout, &format!("smooth-dolt clone from {remote_url}"), SYNC_STALL_HINT)?;
    if !output.status.success() {
        let stderr: String = String::from_utf8_lossy(&output.stderr).trim().chars().take(400).collect();
        anyhow::bail!(
            "smooth-dolt clone from {remote_url} failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr
        );
    }
    Ok(())
}

/// How the local pearl history relates to the remote's `refs/dolt/data`
/// history. Computed by [`classify_remote_sync`] from two bounded
/// `dolt log` outputs — heuristic by construction (a tip older than the
/// log bound looks like a divergence), so callers should phrase findings
/// as "within the last N commits".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSyncStatus {
    /// Tips are equal.
    InSync,
    /// The remote tip appears in local history → safe to `push`.
    LocalAhead,
    /// The local tip appears in remote history → safe to `pull`.
    RemoteAhead,
    /// No overlap, and the remote is exactly one bare
    /// "Initialize data repository" commit — the stray-re-init signature
    /// (2026-07-02 incident). A force push overwrites ONLY that commit.
    DivergedBareInit,
    /// No overlap and the remote has real commits — inspect before any
    /// force push/pull.
    Diverged,
    /// Remote log is empty (nothing ever pushed).
    EmptyRemote,
    /// Local log is empty (fresh/blank store).
    EmptyLocal,
}

/// Pure classification of local vs remote history from `smooth-dolt log`
/// lines (format: `"<short-hash> <message> (<author>) <date>"` — see
/// `cmdLog` in go/smooth-dolt). First line is the tip.
#[must_use]
pub fn classify_remote_sync(local: &[String], remote: &[String]) -> RemoteSyncStatus {
    fn hash(line: &str) -> &str {
        line.split_whitespace().next().unwrap_or("")
    }
    fn message(line: &str) -> &str {
        line.split_once(char::is_whitespace).map_or("", |(_, rest)| rest.trim_start())
    }
    if local.is_empty() {
        return RemoteSyncStatus::EmptyLocal;
    }
    if remote.is_empty() {
        return RemoteSyncStatus::EmptyRemote;
    }
    let local_tip = hash(&local[0]);
    let remote_tip = hash(&remote[0]);
    if local_tip == remote_tip {
        return RemoteSyncStatus::InSync;
    }
    if local.iter().any(|l| hash(l) == remote_tip) {
        return RemoteSyncStatus::LocalAhead;
    }
    if remote.iter().any(|l| hash(l) == local_tip) {
        return RemoteSyncStatus::RemoteAhead;
    }
    if remote.len() == 1 && message(&remote[0]).starts_with("Initialize data repository") {
        return RemoteSyncStatus::DivergedBareInit;
    }
    RemoteSyncStatus::Diverged
}

#[cfg(test)]
mod remote_sync_classify_tests {
    use super::{classify_remote_sync, RemoteSyncStatus};

    fn log(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn in_sync_when_tips_equal() {
        let local = log(&["aaaa1111 close th-1 (brent) 2026-07-01", "bbbb2222 create th-1 (brent) 2026-06-30"]);
        let remote = log(&["aaaa1111 close th-1 (brent) 2026-07-01"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::InSync);
    }

    #[test]
    fn local_ahead_when_remote_tip_in_local_history() {
        let local = log(&["cccc3333 newer (brent) d", "aaaa1111 shared tip (brent) d", "bbbb2222 older (brent) d"]);
        let remote = log(&["aaaa1111 shared tip (brent) d", "bbbb2222 older (brent) d"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::LocalAhead);
    }

    #[test]
    fn remote_ahead_when_local_tip_in_remote_history() {
        let local = log(&["aaaa1111 shared tip (brent) d", "bbbb2222 older (brent) d"]);
        let remote = log(&["dddd4444 teammate work (kim) d", "aaaa1111 shared tip (brent) d"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::RemoteAhead);
    }

    #[test]
    fn diverged_bare_init_single_stray_init_commit() {
        // The 2026-07-02 incident: remote refs/dolt/data re-initialized
        // with a single bare commit, no ancestor with 2547 local commits.
        let local = log(&["aaaa1111 close th-9 (brent) d", "bbbb2222 create th-9 (brent) d"]);
        let remote = log(&["ffff9999 Initialize data repository (dolt) d"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::DivergedBareInit);
    }

    #[test]
    fn diverged_when_remote_has_real_unrelated_commits() {
        let local = log(&["aaaa1111 close th-9 (brent) d"]);
        let remote = log(&["eeee5555 real work (kim) d", "ffff9999 Initialize data repository (dolt) d"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::Diverged);
    }

    #[test]
    fn diverged_when_single_remote_commit_is_not_bare_init() {
        let local = log(&["aaaa1111 close th-9 (brent) d"]);
        let remote = log(&["eeee5555 real work (kim) d"]);
        assert_eq!(classify_remote_sync(&local, &remote), RemoteSyncStatus::Diverged);
    }

    #[test]
    fn empty_remote_log() {
        let local = log(&["aaaa1111 close th-9 (brent) d"]);
        assert_eq!(classify_remote_sync(&local, &[]), RemoteSyncStatus::EmptyRemote);
    }

    #[test]
    fn empty_local_log_wins_over_empty_remote() {
        let remote = log(&["aaaa1111 close th-9 (brent) d"]);
        assert_eq!(classify_remote_sync(&[], &remote), RemoteSyncStatus::EmptyLocal);
        assert_eq!(classify_remote_sync(&[], &[]), RemoteSyncStatus::EmptyLocal);
    }
}

/// The git ref that carries dolt data on a git remote.
pub const DOLT_DATA_REF: &str = "refs/dolt/data";

/// Current tip of `refs/dolt/data` on the remote, via `git ls-remote` —
/// one ref advertisement instead of a full clone. On a 2547-commit store
/// the probe clone (`clone_from_bounded`) is ~5 minutes at 96% CPU and
/// brushes up against the default 300s sync bound; ls-remote answers in
/// ~1s, so `th pearls doctor` runs this tip-level check first (pearl
/// th-c42cc4).
///
/// Dolt stores git remotes in `git+ssh://` / `git+file://` form — git
/// itself doesn't accept the `git+` prefix, so it's stripped here.
/// Bounded by the standard remote-sync timeout ([`sync_timeout`]).
///
/// Returns `Ok(None)` when the remote is reachable but has no
/// `refs/dolt/data` (never pushed).
///
/// # Errors
/// - git unreachable / bad URL / auth failure (non-zero exit)
/// - the ls-remote exceeded the sync bound (matches [`is_sync_timeout_err`])
pub fn remote_dolt_data_tip(remote_url: &str) -> Result<Option<String>> {
    let url = remote_url.strip_prefix("git+").unwrap_or(remote_url);
    let child = Command::new("git")
        .args(["ls-remote", url, DOLT_DATA_REF])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn git ls-remote {url}"))?;
    let output = wait_child_draining(child, sync_timeout(), &format!("git ls-remote {url}"), SYNC_STALL_HINT)?;
    if !output.status.success() {
        let stderr: String = String::from_utf8_lossy(&output.stderr).trim().chars().take(300).collect();
        anyhow::bail!(
            "git ls-remote {url} failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() { "(no stderr)" } else { stderr.as_str() }
        );
    }
    // Output: "<hash>\trefs/dolt/data\n", or empty when the ref doesn't exist.
    Ok(String::from_utf8_lossy(&output.stdout).split_whitespace().next().map(str::to_string))
}

/// The `refs/dolt/data` tip recorded locally at the last successful sync.
///
/// The enclosing git repo does NOT carry a local `refs/dolt/data`
/// (verified: `git show-ref` lists nothing under `refs/dolt/` even on a
/// store that syncs daily). Dolt's git blobstore instead keeps a bare
/// repo cache per remote at `<db>/.dolt/git-remote-cache/<key>/repo.git`,
/// whose `FETCH_HEAD` records the remote's `refs/dolt/data` tip as of the
/// last fetch — and both push and pull fetch (push does a
/// check-and-put of the manifest), so the line tracks the last sync in
/// either direction.
///
/// `None` when the cache is absent (never synced from this checkout;
/// also the case for `file://` remotes, which skip the cache) or when
/// multiple cache repos disagree — callers fall back to the deep probe.
#[must_use]
pub fn last_synced_dolt_data_tip(db_dir: &Path) -> Option<String> {
    let cache = db_dir.join(".dolt").join("git-remote-cache");
    let mut tips: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(cache).ok()?.flatten() {
        let Ok(raw) = std::fs::read_to_string(entry.path().join("repo.git").join("FETCH_HEAD")) else {
            continue;
        };
        // FETCH_HEAD line: "<hash>\t\t'refs/dolt/data' of ssh://github.com/Org/repo"
        for line in raw.lines().filter(|l| l.contains(&format!("'{DOLT_DATA_REF}'"))) {
            if let Some(h) = line.split_whitespace().next() {
                tips.push(h.to_string());
            }
        }
    }
    tips.sort();
    tips.dedup();
    match tips.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Hash of a named branch from `smooth-dolt sql` JSON rows
/// (`select name, hash from dolt_branches` / `dolt_remote_branches`).
#[must_use]
pub fn branch_hash(rows: &[Value], name: &str) -> Option<String> {
    rows.iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|r| r.get("hash").and_then(Value::as_str))
        .map(str::to_string)
}

/// Verdict of the cheap tip-level sync check that gates the doctor's
/// deep probe clone. Computed by [`classify_tip_check`] from four
/// signals, all obtainable without cloning the remote:
///
/// - local dolt branch head (`dolt_branches`)
/// - dolt remote-tracking head (`dolt_remote_branches`, updated on both
///   push and pull — verified against a scratch store)
/// - last-synced git tip ([`last_synced_dolt_data_tip`])
/// - current remote git tip ([`remote_dolt_data_tip`])
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipCheck {
    /// No local commits since the last sync AND the remote ref hasn't
    /// moved since we last saw it → in sync, no clone needed.
    InSync,
    /// Local head differs from the remote-tracking head → local commits
    /// since the last sync.
    LocalMoved,
    /// The remote's `refs/dolt/data` differs from the tip we last synced.
    RemoteMoved,
    /// A signal is missing (never synced, no cache, no remote ref) —
    /// only the deep probe can classify.
    Unknown,
}

/// Pure decision for the tip-level check. Any missing signal is
/// [`TipCheck::Unknown`] (fail toward the deep probe, never toward a
/// false "in sync"). When both sides moved, `LocalMoved` is reported —
/// the deep probe runs in every non-`InSync` case anyway.
#[must_use]
pub fn classify_tip_check(local_head: Option<&str>, tracking_head: Option<&str>, last_synced_tip: Option<&str>, remote_tip: Option<&str>) -> TipCheck {
    let (Some(local), Some(tracking), Some(synced), Some(remote)) = (local_head, tracking_head, last_synced_tip, remote_tip) else {
        return TipCheck::Unknown;
    };
    if local != tracking {
        return TipCheck::LocalMoved;
    }
    if synced != remote {
        return TipCheck::RemoteMoved;
    }
    TipCheck::InSync
}

#[cfg(test)]
mod tip_check_tests {
    use super::{branch_hash, classify_tip_check, last_synced_dolt_data_tip, remote_dolt_data_tip, TipCheck, DOLT_DATA_REF};
    use serde_json::json;
    use std::path::Path;

    // ---- classify_tip_check (pure) ----

    #[test]
    fn in_sync_when_all_four_signals_align() {
        assert_eq!(classify_tip_check(Some("d1"), Some("d1"), Some("g1"), Some("g1")), TipCheck::InSync);
    }

    #[test]
    fn local_moved_when_head_diverges_from_tracking() {
        assert_eq!(classify_tip_check(Some("d2"), Some("d1"), Some("g1"), Some("g1")), TipCheck::LocalMoved);
    }

    #[test]
    fn remote_moved_when_remote_tip_differs_from_last_synced() {
        assert_eq!(classify_tip_check(Some("d1"), Some("d1"), Some("g1"), Some("g2")), TipCheck::RemoteMoved);
    }

    #[test]
    fn local_moved_wins_when_both_sides_moved() {
        assert_eq!(classify_tip_check(Some("d2"), Some("d1"), Some("g1"), Some("g2")), TipCheck::LocalMoved);
    }

    #[test]
    fn unknown_when_any_signal_missing() {
        assert_eq!(classify_tip_check(None, Some("d1"), Some("g1"), Some("g1")), TipCheck::Unknown);
        assert_eq!(classify_tip_check(Some("d1"), None, Some("g1"), Some("g1")), TipCheck::Unknown);
        assert_eq!(classify_tip_check(Some("d1"), Some("d1"), None, Some("g1")), TipCheck::Unknown);
        assert_eq!(classify_tip_check(Some("d1"), Some("d1"), Some("g1"), None), TipCheck::Unknown);
    }

    // ---- branch_hash (pure) ----

    #[test]
    fn branch_hash_finds_named_branch() {
        let rows = vec![json!({"name": "other", "hash": "aaa"}), json!({"name": "main", "hash": "bbb"})];
        assert_eq!(branch_hash(&rows, "main"), Some("bbb".to_string()));
        assert_eq!(branch_hash(&rows, "missing"), None);
        assert_eq!(branch_hash(&[], "main"), None);
    }

    // ---- last_synced_dolt_data_tip (fixture) ----

    fn write_fetch_head(db_dir: &Path, cache_key: &str, contents: &str) {
        let repo = db_dir.join(".dolt").join("git-remote-cache").join(cache_key).join("repo.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("FETCH_HEAD"), contents).unwrap();
    }

    #[test]
    fn last_synced_tip_read_from_fetch_head() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_fetch_head(
            tmp.path(),
            "1b5d",
            "aaaa1111\t\t'refs/heads/main' of ssh://github.com/Org/repo\n4f3a8033\t\t'refs/dolt/data' of ssh://github.com/Org/repo\n",
        );
        assert_eq!(last_synced_dolt_data_tip(tmp.path()), Some("4f3a8033".to_string()));
    }

    #[test]
    fn last_synced_tip_none_without_cache_or_matching_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(last_synced_dolt_data_tip(tmp.path()), None, "no git-remote-cache at all");

        write_fetch_head(tmp.path(), "1b5d", "aaaa1111\t\t'refs/heads/main' of ssh://github.com/Org/repo\n");
        assert_eq!(last_synced_dolt_data_tip(tmp.path()), None, "FETCH_HEAD without a refs/dolt/data line");
    }

    #[test]
    fn last_synced_tip_multiple_caches_agree_or_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_fetch_head(tmp.path(), "aaaa", "1111\t\t'refs/dolt/data' of ssh://h/r\n");
        write_fetch_head(tmp.path(), "bbbb", "1111\t\t'refs/dolt/data' of ssh://h/r\n");
        assert_eq!(
            last_synced_dolt_data_tip(tmp.path()),
            Some("1111".to_string()),
            "agreeing caches are unambiguous"
        );

        write_fetch_head(tmp.path(), "cccc", "2222\t\t'refs/dolt/data' of ssh://h/r\n");
        assert_eq!(last_synced_dolt_data_tip(tmp.path()), None, "disagreeing caches are ambiguous");
    }

    // ---- remote_dolt_data_tip (local git fixture) ----

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn fixture_repo_with_commit() -> (tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q"]);
        git(
            tmp.path(),
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-q", "-m", "x"],
        );
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        (tmp, head)
    }

    #[test]
    fn remote_tip_found_via_ls_remote() {
        let (tmp, head) = fixture_repo_with_commit();
        git(tmp.path(), &["update-ref", DOLT_DATA_REF, &head]);
        // Plain path form.
        let tip = remote_dolt_data_tip(&tmp.path().to_string_lossy()).unwrap();
        assert_eq!(tip, Some(head.clone()));
        // The git+file:// form dolt stores — the git+ prefix must be stripped.
        let tip = remote_dolt_data_tip(&format!("git+file://{}", tmp.path().display())).unwrap();
        assert_eq!(tip, Some(head));
    }

    #[test]
    fn remote_tip_none_when_ref_absent() {
        let (tmp, _head) = fixture_repo_with_commit();
        assert_eq!(remote_dolt_data_tip(&tmp.path().to_string_lossy()).unwrap(), None);
    }

    #[test]
    fn remote_tip_errors_on_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = remote_dolt_data_tip(&tmp.path().to_string_lossy()).unwrap_err();
        assert!(format!("{err:#}").contains("ls-remote"), "error names the failing operation: {err:#}");
    }
}

/// Normalize a git remote URL for Dolt's remote machinery.
///
/// SCP-style SSH URLs (`user@host:path`) are not real URLs — the colon
/// separates the host from a RELATIVE path. Dolt's own URL parser
/// mishandles them, storing `git@github.com:SmooAI/smooth.git` as
/// `git+ssh://git@github.com/./SmooAI/smooth.git` (bogus `/./`), which
/// then fails on push/pull. Convert them ourselves to the clean form
/// Dolt stores for working remotes: `git+ssh://user@host/path`.
/// Everything that is already a URL (`https://`, `ssh://`, `git+ssh://`)
/// or a filesystem path passes through unchanged. Pearl th-c4441b.
fn normalize_remote_url(url: &str) -> String {
    // Anything with an explicit scheme is already a real URL.
    if url.contains("://") {
        return url.to_string();
    }
    // SCP-style: `user@host:path`. Require the `@` and a colon before any
    // slash so filesystem paths (absolute, relative, or colon-bearing)
    // never match. ponytail: `host:path` without a user passes through —
    // ambiguous with local paths, and nobody clones pearls that way.
    let Some((head, path)) = url.split_once(':') else {
        return url.to_string();
    };
    if path.is_empty() || head.contains('/') || !head.contains('@') {
        return url.to_string();
    }
    // `user@host:/abs/path` → keep the path absolute; relative paths get
    // a single separating slash. Both collapse to `git+ssh://head/path`.
    let path = path.strip_prefix('/').unwrap_or(path);
    format!("git+ssh://{head}/{path}")
}

/// Is this a stored remote URL that Dolt mangled with the bogus `/./`
/// segment? Returns the repaired URL.
///
/// [`normalize_remote_url`] stops NEW stores from getting one, but every
/// store created before that fix still has the broken URL persisted in
/// `repo_state.json` — and nothing repaired it. The consequence is
/// nastier than a cosmetic typo: `git` rejects the path
/// (`./SmooAI/smooai is not a valid repository name`), the
/// `smooth-dolt push` never returns, and the hung push holds the noms
/// write lock — so EVERY writer of that store gets `Error 1105: cannot
/// update manifest: database is read only` while reads keep working.
/// One bad URL join wedges every agent's pearl and mail writes.
#[must_use]
pub fn repair_malformed_remote_url(url: &str) -> Option<String> {
    url.contains("/./").then(|| url.replace("/./", "/"))
}

impl SmoothDolt {
    /// The `origin` URL Dolt has recorded for this store, if any.
    ///
    /// # Errors
    /// No `repo_state.json`, or no `origin` remote in it.
    pub fn origin_url(&self) -> Result<String> {
        read_origin_url(&self.data_dir)
    }

    /// Repoint `origin` at the repaired URL when it carries the `/./`
    /// mangling. Returns the new URL, or `None` when the remote was
    /// already fine.
    ///
    /// Touches only the remote pointer — never history. No push, no
    /// fetch, no re-clone.
    pub fn repair_origin_remote(&self) -> Result<Option<String>> {
        let current = read_origin_url(&self.data_dir)?;
        let Some(fixed) = repair_malformed_remote_url(&current) else {
            return Ok(None);
        };
        self.exec("CALL DOLT_REMOTE('remove', 'origin')").context("drop the malformed origin remote")?;
        self.remote_add("origin", &fixed).context("re-add origin with the repaired URL")?;
        Ok(Some(fixed))
    }
}

/// Read the `origin` remote URL from `<data_dir>/.dolt/repo_state.json`.
fn read_origin_url(data_dir: &std::path::Path) -> Result<String> {
    let path = data_dir.join(".dolt").join("repo_state.json");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    let url = v
        .get("remotes")
        .and_then(|r| r.get("origin"))
        .and_then(|o| o.get("url"))
        .and_then(|u| u.as_str())
        .context("repo_state.json: missing remotes.origin.url")?;
    Ok(url.to_string())
}

/// Read the enclosing git repository's `origin` URL via
/// `git -C <start> remote get-url origin`. Recovery fallback for when
/// the dolt `repo_state.json` (which normally records the remote) has
/// been wiped by an interrupted operation. Returned verbatim; SCP-style
/// URLs (e.g. `git@github.com:Org/repo.git`) are normalized to
/// `git+ssh://` form by [`clone_from`] / [`SmoothDolt::remote_add`]
/// before Dolt sees them (pearl th-c4441b).
fn read_git_origin_url(start: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["remote", "get-url", "origin"])
        .output()
        .context("exec git remote get-url origin")?;
    if !output.status.success() {
        anyhow::bail!("git remote get-url origin failed in {} (no origin remote?)", start.display());
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        anyhow::bail!("git origin url is empty");
    }
    Ok(url)
}

#[cfg(test)]
mod auto_heal_tests {
    use super::{read_git_origin_url, DoctorDiagnosis, SmoothDolt};
    use std::process::Command;

    /// `.dolt/` present but `noms/manifest` + `repo_state.json` gone =
    /// the interrupted-GC/half-clone signature → recoverable Corrupt,
    /// not a dead-end NotInitialized. This must classify before the cold
    /// `log` probe so it never depends on the smooth-dolt binary. Pearl
    /// th-03cdb8 / the 2026-06-18 incident.
    #[test]
    fn diagnose_missing_core_files_is_corrupt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".dolt").join("noms")).unwrap();
        // Deliberately write neither manifest nor repo_state.json.

        match SmoothDolt::diagnose(dir) {
            DoctorDiagnosis::Corrupt { detail } => {
                assert!(detail.contains("noms/manifest"), "detail names manifest: {detail}");
                assert!(detail.contains("repo_state.json"), "detail names repo_state: {detail}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    /// Only the manifest is missing (repo_state present) — still Corrupt.
    #[test]
    fn diagnose_missing_manifest_only_is_corrupt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".dolt").join("noms")).unwrap();
        std::fs::write(dir.join(".dolt").join("repo_state.json"), "{}").unwrap();

        match SmoothDolt::diagnose(dir) {
            DoctorDiagnosis::Corrupt { detail } => {
                assert!(detail.contains("noms/manifest"), "detail: {detail}");
                assert!(!detail.contains("repo_state.json"), "repo_state present, should not be listed: {detail}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    /// A git-conflict-marker manifest is reported as ConflictMarkers
    /// (hand-resolvable) and takes priority over the missing-files check.
    #[test]
    fn diagnose_conflict_markers_take_priority() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".dolt").join("noms")).unwrap();
        std::fs::write(
            dir.join(".dolt").join("noms").join("manifest"),
            "<<<<<<< HEAD\n5:__DOLT__:aaa:bbb\n=======\n5:__DOLT__:ccc:ddd\n>>>>>>> other\n",
        )
        .unwrap();
        match SmoothDolt::diagnose(dir) {
            DoctorDiagnosis::ConflictMarkers { candidates } => {
                assert_eq!(candidates.len(), 2, "two non-marker candidate lines");
            }
            other => panic!("expected ConflictMarkers, got {other:?}"),
        }
    }

    #[test]
    fn read_git_origin_url_reads_configured_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:SmooAI/smooth.git"]);
        assert_eq!(read_git_origin_url(dir).unwrap(), "git@github.com:SmooAI/smooth.git");
    }

    #[test]
    fn read_git_origin_url_errors_without_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        Command::new("git").arg("-C").arg(dir).args(["init", "-q"]).output().unwrap();
        assert!(read_git_origin_url(dir).is_err());
    }
}

#[cfg(test)]
mod sql_escape_tests {
    use super::sql_escape;

    #[test]
    fn empty_string_unchanged() {
        assert_eq!(sql_escape(""), "");
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(sql_escape("hello world"), "hello world");
    }

    #[test]
    fn single_quote_doubled() {
        assert_eq!(sql_escape("it's"), "it''s");
        assert_eq!(sql_escape("''"), "''''");
    }

    #[test]
    fn backslash_doubled() {
        assert_eq!(sql_escape(r"a\b"), r"a\\b");
    }

    #[test]
    fn backslash_quote_the_th_944230_case() {
        // `\'` must become `\\''` — backslash escaped BEFORE the quote is
        // doubled, so the backslash can't eat the quote.
        assert_eq!(sql_escape(r"text with \' inside"), r"text with \\'' inside");
    }

    #[test]
    fn lone_trailing_backslash() {
        // `abc\` unescaped would eat the literal's closing quote.
        assert_eq!(sql_escape(r"abc\"), r"abc\\");
    }

    #[test]
    fn doubled_backslashes() {
        assert_eq!(sql_escape(r"a\\b"), r"a\\\\b");
    }

    #[test]
    fn nul_byte_escaped() {
        assert_eq!(sql_escape("a\0b"), r"a\0b");
    }

    #[test]
    fn classic_injection_payload_neutralized() {
        let escaped = sql_escape("'; DROP TABLE pearls; --");
        // No lone quote survives: the only quotes are the doubled pair.
        assert_eq!(escaped, "''; DROP TABLE pearls; --");
        assert!(!escaped.contains('\\'));
    }

    #[test]
    fn quote_backslash_injection_neutralized() {
        // The bypass the old quotes-only escape allowed.
        assert_eq!(sql_escape(r"\'; DROP TABLE pearls; --"), r"\\''; DROP TABLE pearls; --");
    }

    #[test]
    fn semicolons_and_newlines_pass_through() {
        assert_eq!(sql_escape("a;b\nc\r\nd"), "a;b\nc\r\nd");
    }

    #[test]
    fn unicode_pass_through() {
        assert_eq!(sql_escape("héllo 世界 🦀"), "héllo 世界 🦀");
    }
}

#[cfg(test)]
mod normalize_remote_url_tests {
    use super::normalize_remote_url;

    /// Regression for pearl th-c4441b: `th pearls remote add` handed the
    /// SCP form straight to Dolt, which stored the broken
    /// `git+ssh://git@github.com/./SmooAI/smooth.git` (bogus `/./`).
    #[test]
    fn th_c4441b_scp_github_url_converts_cleanly() {
        assert_eq!(
            normalize_remote_url("git@github.com:SmooAI/smooth.git"),
            "git+ssh://git@github.com/SmooAI/smooth.git"
        );
    }

    #[test]
    fn scp_relative_path() {
        assert_eq!(normalize_remote_url("git@example.com:some/repo.git"), "git+ssh://git@example.com/some/repo.git");
    }

    #[test]
    fn scp_absolute_path() {
        assert_eq!(
            normalize_remote_url("git@host.example:/srv/git/repo.git"),
            "git+ssh://git@host.example/srv/git/repo.git"
        );
    }

    #[test]
    fn scp_without_dot_git_suffix() {
        assert_eq!(normalize_remote_url("git@github.com:SmooAI/smooth"), "git+ssh://git@github.com/SmooAI/smooth");
    }

    #[test]
    fn scp_numeric_looking_path_segment_is_a_path_not_a_port() {
        // In SCP form everything after the colon is a path — git itself
        // treats `host:2222/repo` as the path `2222/repo`, never a port.
        assert_eq!(
            normalize_remote_url("git@host.example:2222/repo.git"),
            "git+ssh://git@host.example/2222/repo.git"
        );
    }

    #[test]
    fn https_url_passes_through() {
        assert_eq!(
            normalize_remote_url("https://github.com/SmooAI/smooth.git"),
            "https://github.com/SmooAI/smooth.git"
        );
    }

    #[test]
    fn ssh_url_passes_through() {
        assert_eq!(
            normalize_remote_url("ssh://git@github.com/SmooAI/smooth.git"),
            "ssh://git@github.com/SmooAI/smooth.git"
        );
    }

    #[test]
    fn ssh_url_with_port_passes_through() {
        assert_eq!(
            normalize_remote_url("ssh://git@host.example:2222/srv/repo.git"),
            "ssh://git@host.example:2222/srv/repo.git"
        );
    }

    #[test]
    fn git_ssh_url_passes_through() {
        assert_eq!(
            normalize_remote_url("git+ssh://git@github.com/SmooAI/smooth.git"),
            "git+ssh://git@github.com/SmooAI/smooth.git"
        );
    }

    #[test]
    fn file_url_passes_through() {
        assert_eq!(normalize_remote_url("file:///home/user/repo"), "file:///home/user/repo");
    }

    #[test]
    fn local_paths_pass_through() {
        assert_eq!(normalize_remote_url("/home/user/repo"), "/home/user/repo");
        assert_eq!(normalize_remote_url("./relative/repo"), "./relative/repo");
        assert_eq!(normalize_remote_url("../up/repo"), "../up/repo");
        assert_eq!(normalize_remote_url("plain-dir"), "plain-dir");
    }

    #[test]
    fn colon_bearing_paths_without_user_pass_through() {
        // No `@` before the colon → ambiguous with a local path; leave it.
        assert_eq!(normalize_remote_url("host:path"), "host:path");
        // `@` present but a slash before the colon → local path, not SCP.
        assert_eq!(normalize_remote_url("dir/with@at:colon"), "dir/with@at:colon");
        // Trailing colon with nothing after it → not a usable SCP URL.
        assert_eq!(normalize_remote_url("git@github.com:"), "git@github.com:");
    }
}
