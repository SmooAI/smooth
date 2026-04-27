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

/// Handle to the smooth-dolt binary. All Dolt operations go through this.
///
/// Two transports are supported:
///
/// 1. **CLI mode** (default — [`SmoothDolt::new`]): each method spawns a
///    fresh `smooth-dolt sql ...` subprocess via `Command::output`.
///    Works fine for short-lived commands like `th pearls list`.
///
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
    /// Create a CLI-mode handle pointing at the given data directory.
    /// Locates the `smooth-dolt` binary automatically.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
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

    /// Wrap a server-mode op with one round of self-healing on
    /// transport-looking errors (broken pipe, timeout, EOF). The
    /// `client()` path already retries connect failures; this catches
    /// the case where connect succeeds but the request itself wedges
    /// (e.g. dolt mid-deadlock from a paused volume after sleep).
    fn run_with_self_heal<T>(server: &Arc<SmoothDoltServer>, op: impl Fn(&Arc<SmoothDoltServer>) -> Result<T>) -> Result<T> {
        match op(server) {
            Ok(v) => Ok(v),
            Err(e) if is_transport_err(&e) => {
                tracing::warn!(error = %e, "smooth-dolt op looked like a transport failure; respawning + retrying once");
                server.ensure_healthy().context("self-heal: ensure_healthy")?;
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

    /// Execute a SQL query and return parsed JSON results.
    pub fn sql(&self, query: &str) -> Result<Vec<Value>> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.client()?.sql(query));
        }
        let output = self.run_cli(&["sql", &self.data_dir_str(), "-q", query])?;
        if output.is_empty() || output == "null" {
            return Ok(Vec::new());
        }
        let parsed: Vec<Value> = serde_json::from_str(&output).with_context(|| format!("parse smooth-dolt sql output: {output}"))?;
        Ok(parsed)
    }

    /// Execute a SQL statement (INSERT/UPDATE/DELETE/CREATE). Returns raw output.
    pub fn exec(&self, statement: &str) -> Result<String> {
        if let Some(server) = &self.server {
            let rows = Self::run_with_self_heal(server, |s| s.client()?.exec(statement))?;
            return Ok(format!("{rows} rows affected"));
        }
        self.run_cli(&["sql", &self.data_dir_str(), "-q", statement])
    }

    /// Stage all changes and commit with a message.
    pub fn commit(&self, message: &str) -> Result<String> {
        if let Some(server) = &self.server {
            return Self::run_with_self_heal(server, |s| s.client()?.commit(message));
        }
        self.run_cli(&["commit", &self.data_dir_str(), "-m", message])
    }

    /// Query the Dolt commit log. Returns vec of (hash, author, date, message).
    pub fn log(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        let output = if let Some(server) = &self.server {
            server.client()?.log(limit)?
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

    /// Push to the configured Dolt remote (refs/dolt/data on git origin).
    pub fn push(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return server.client()?.dolt("push");
        }
        self.run_cli(&["push", &self.data_dir_str()])
    }

    /// Pull from the configured Dolt remote.
    pub fn pull(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return server.client()?.dolt("pull");
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
            return server.client()?.dolt("gc");
        }
        self.run_cli(&["gc", &self.data_dir_str()])
    }

    /// Check the Dolt status (working set changes).
    pub fn status(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return server.client()?.dolt("status");
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
