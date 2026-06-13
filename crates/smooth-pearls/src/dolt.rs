//! smooth-dolt subprocess wrapper.
//!
//! Provides a clean Rust interface to the `smooth-dolt` Go binary for
//! all Dolt operations (init, SQL, commit, push, pull, log, remote, gc).
//! The binary is located once at startup and reused for all calls.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::dolt_server::SmoothDoltServer;

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
                tracing::warn!(error = %e, "smooth-dolt op looked like a lock wedge (db read-only); force-respawning + retrying once");
                server.force_respawn().context("self-heal: force_respawn")?;
                op(server)
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
        self.run_cli(&args)
    }

    /// Pull from the configured Dolt remote.
    pub fn pull(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.with_client(|c| c.dolt("pull")));
        }
        self.run_cli(&["pull", &self.data_dir_str()])
    }

    /// Add a Dolt remote. CLI-only; the server protocol doesn't expose
    /// remote management because it's an administrative one-shot.
    pub fn remote_add(&self, name: &str, url: &str) -> Result<String> {
        if self.server.is_some() {
            anyhow::bail!("remote_add is not supported in server mode; use the CLI directly");
        }
        self.run_cli(&["remote", &self.data_dir_str(), "add", name, url])
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
        match self.run_cli_once(args) {
            Ok(v) => Ok(v),
            Err(e) if is_lock_wedge_err(&e) => {
                // Pearl th-49e37b: in CLI mode the server-mode
                // self-heal in `run_with_self_heal` doesn't fire, so
                // the read-only error propagates straight up. The
                // root cause we see most often is an orphaned
                // `smooth-dolt serve` that's still holding the LOCK
                // file even though its socket file (the way
                // `try_attach_handle` finds it) has been cleaned up
                // — process is reparented to init, no one will ever
                // close it. Detect, kill, retry once.
                match auto_doctor_clear_orphan_server(&self.data_dir) {
                    Ok(cleared) if cleared > 0 => {
                        tracing::warn!(
                            data_dir = %self.data_dir.display(),
                            cleared,
                            "smooth-dolt CLI hit read-only; cleared orphaned `smooth-dolt serve` PID(s) and retrying once"
                        );
                        self.run_cli_once(args)
                    }
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// One-shot CLI invocation. Wrapped by `run_cli` with the
    /// auto-doctor retry — that's the public entry point. This bare
    /// version lives separately so the doctor's retry path can call
    /// it without re-entering the doctor and looping forever.
    fn run_cli_once(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("exec smooth-dolt {}: {}", args.join(" "), self.bin.display()))?;

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

/// Auto-doctor — find `smooth-dolt serve` processes holding the LOCK
/// file under `data_dir/.dolt/noms/LOCK` and kill them. Returns the
/// number of orphan PIDs cleared (0 if none found, which is the
/// normal happy-path case where the read-only error came from a
/// different cause and we should propagate it).
///
/// Pearl th-49e37b. The shape of the bug we're fixing: an earlier
/// `th up` spawned `smooth-dolt serve <data-dir> --socket <path>` as
/// a child. The parent died (e.g. `th down`) but the serve child got
/// reparented to init and the socket file got cleaned up, leaving the
/// serve process running with no way to reach it. It still holds the
/// noms LOCK file. `try_attach_handle` does `socket.exists()` →
/// returns None (file gone) → `SmoothDolt::new` falls back to CLI
/// mode → CLI `smooth-dolt exec` tries to grab the lock → fails with
/// `Error 1105: cannot update manifest: database is read only`.
///
/// SIGTERM the orphan; the OS releases its file locks on death; the
/// retry succeeds. Best-effort: any errors in the doctor itself (e.g.
/// `lsof` not on PATH) silently return 0 so we fall through to the
/// original read-only error rather than masking a real bug.
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
        // Verify the holder is actually `smooth-dolt serve` BEFORE
        // killing — we don't want to accidentally kill a debugger or
        // a backup tool that happened to open the file. `ps -p <pid>
        // -o command=` prints the command line.
        let ps_out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]).output();
        let Ok(ps_out) = ps_out else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&ps_out.stdout);
        if !cmdline.contains("smooth-dolt") || !cmdline.contains("serve") {
            tracing::warn!(
                pid,
                cmdline = %cmdline.trim(),
                "auto_doctor: process holds the dolt LOCK file but is not `smooth-dolt serve` — refusing to kill"
            );
            continue;
        }

        // SIGTERM is the right escalation: gives the server's
        // graceful-shutdown path a chance to fire if it's running,
        // and releases file locks when the process dies. If we ever
        // need a SIGKILL fallback (process truly stuck and ignoring
        // SIGTERM), add a poll-then-escalate here.
        tracing::warn!(pid, "auto_doctor: SIGTERM orphaned `smooth-dolt serve` holding noms LOCK");
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        cleared += 1;
    }

    if cleared > 0 {
        // Give the OS a moment to actually release the locks. Without
        // this the retry races the kernel's fd-cleanup pass and we
        // get a second false read-only error.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    Ok(cleared)
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
        let data_dir = &self.data_dir;
        let remote_url = read_origin_url(data_dir).context("no `origin` remote in repo_state.json — manual `dolt clone <url> <dir>` required")?;
        let parent = data_dir.parent().context("data_dir has no parent")?;
        let leaf = data_dir.file_name().and_then(|n| n.to_str()).context("data_dir has no leaf name")?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let broken_path = parent.join(format!("{leaf}.broken-{ts}"));
        std::fs::rename(data_dir, &broken_path).with_context(|| format!("snapshot corrupt dir → {}", broken_path.display()))?;

        let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found for clone — Run: scripts/build-smooth-dolt.sh")?;
        let output = Command::new(&bin)
            .args(["clone", &remote_url, &data_dir.to_string_lossy()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("exec smooth-dolt clone")?;
        if !output.status.success() {
            // Restore the broken dir so the user isn't stranded.
            let _ = std::fs::rename(&broken_path, data_dir);
            let stderr: String = String::from_utf8_lossy(&output.stderr).trim().chars().take(400).collect();
            anyhow::bail!("smooth-dolt clone failed (exit {}): {}", output.status.code().unwrap_or(-1), stderr);
        }
        Ok(broken_path)
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
    let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found for clone — Run: scripts/build-smooth-dolt.sh")?;
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create parent of {}", target_dir.display()))?;
    }
    let output = Command::new(&bin)
        .args(["clone", remote_url, &target_dir.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("exec smooth-dolt clone")?;
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
