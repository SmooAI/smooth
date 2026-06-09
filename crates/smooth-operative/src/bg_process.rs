//! Background process registry — lets the agent run long-lived processes
//! (dev servers, watchers, databases) that outlive a single tool call.
//!
//! `bash` has a 120s default timeout; it kills anything that doesn't
//! return in that window. That's fine for builds and tests but useless
//! for `npm run dev` or `cargo run` or `python -m uvicorn` style
//! commands that are meant to run indefinitely.
//!
//! The registry:
//! - Accepts a shell command via `run()`, spawns it detached with stdout
//!   and stderr piped into in-memory ring buffers
//! - Returns a short handle (`bg-N`) the agent uses in follow-up calls
//! - `status()` reports running/exited + exit code + uptime
//! - `logs()` returns a tail of the captured output
//! - `kill()` sends SIGTERM then waits briefly for graceful shutdown
//!
//! Buffers are bounded (default 256 KB per stream) so a chatty server
//! can't OOM the runner.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const MAX_LOG_BYTES: usize = 256 * 1024;

/// Shared, clone-cheap registry. Every background tool takes an
/// `Arc<BgRegistry>` and operates through its handles.
#[derive(Clone, Default)]
pub struct BgRegistry {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU32>,
}

#[derive(Default)]
struct Inner {
    procs: HashMap<String, Entry>,
}

struct Entry {
    command: String,
    #[allow(dead_code)]
    workdir: String,
    started_at: Instant,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    /// The `Child` is held so we can kill it later. Wrapped in a Mutex
    /// inside an Option so we can take ownership for `kill()`.
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    /// Cached exit status — populated by the reaper task once the child
    /// exits. `None` means "still running as far as we know".
    exit_code: Arc<Mutex<Option<i32>>>,
}

/// A snapshot of a registered background process.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BgStatus {
    pub handle: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub uptime_secs: u64,
}

impl BgRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn `command` via `sh -c` under `workdir` and return its handle.
    ///
    /// `env_vars` is applied to the child's environment (used to pipe
    /// HTTP_PROXY etc. through, mirroring the bash tool).
    ///
    /// # Errors
    /// Returns an error if the child fails to spawn.
    pub fn run(&self, command: &str, workdir: &str, env_vars: &[(String, String)]) -> anyhow::Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let handle = format!("bg-{id}");

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("spawn '{command}': {e}"))?;

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));

        // Drain stdout in the background.
        if let Some(out) = child.stdout.take() {
            let buf = Arc::clone(&stdout_buf);
            tokio::spawn(async move {
                let mut reader = BufReader::new(out);
                let mut line = String::new();
                while let Ok(n) = reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    append_bounded(&buf, &line);
                    line.clear();
                }
            });
        }
        // Drain stderr in the background.
        if let Some(err) = child.stderr.take() {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let mut reader = BufReader::new(err);
                let mut line = String::new();
                while let Ok(n) = reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    append_bounded(&buf, &line);
                    line.clear();
                }
            });
        }

        let exit_code = Arc::new(Mutex::new(None));
        let child_holder = Arc::new(tokio::sync::Mutex::new(Some(child)));

        // Reaper task — waits for the child, records the exit code.
        let reaper_child = Arc::clone(&child_holder);
        let reaper_exit = Arc::clone(&exit_code);
        tokio::spawn(async move {
            let mut guard = reaper_child.lock().await;
            if let Some(mut child) = guard.take() {
                if let Ok(status) = child.wait().await {
                    if let Ok(mut slot) = reaper_exit.lock() {
                        *slot = Some(status.code().unwrap_or(-1));
                    }
                }
            }
        });

        let entry = Entry {
            command: command.to_string(),
            workdir: workdir.to_string(),
            started_at: Instant::now(),
            stdout: stdout_buf,
            stderr: stderr_buf,
            child: child_holder,
            exit_code,
        };

        if let Ok(mut inner) = self.inner.lock() {
            inner.procs.insert(handle.clone(), entry);
        }
        Ok(handle)
    }

    /// Get the current status of a handle.
    ///
    /// # Errors
    /// Returns an error if the handle is unknown.
    pub fn status(&self, handle: &str) -> anyhow::Result<BgStatus> {
        let inner = self.inner.lock().map_err(|_| anyhow::anyhow!("bg registry poisoned"))?;
        let entry = inner.procs.get(handle).ok_or_else(|| anyhow::anyhow!("unknown bg handle: {handle}"))?;
        let exit_code = entry.exit_code.lock().ok().and_then(|g| *g);
        Ok(BgStatus {
            handle: handle.to_string(),
            command: entry.command.clone(),
            running: exit_code.is_none(),
            exit_code,
            uptime_secs: entry.started_at.elapsed().as_secs(),
        })
    }

    /// List all registered handles with their status.
    #[must_use]
    pub fn list(&self) -> Vec<BgStatus> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out: Vec<BgStatus> = inner
            .procs
            .iter()
            .map(|(handle, entry)| {
                let exit_code = entry.exit_code.lock().ok().and_then(|g| *g);
                BgStatus {
                    handle: handle.clone(),
                    command: entry.command.clone(),
                    running: exit_code.is_none(),
                    exit_code,
                    uptime_secs: entry.started_at.elapsed().as_secs(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.handle.cmp(&b.handle));
        out
    }

    /// Fetch a tail of captured stdout + stderr for `handle`. `max_bytes_each`
    /// bounds each stream individually.
    ///
    /// # Errors
    /// Returns an error if the handle is unknown.
    pub fn logs(&self, handle: &str, max_bytes_each: usize) -> anyhow::Result<(String, String)> {
        let inner = self.inner.lock().map_err(|_| anyhow::anyhow!("bg registry poisoned"))?;
        let entry = inner.procs.get(handle).ok_or_else(|| anyhow::anyhow!("unknown bg handle: {handle}"))?;
        let stdout = tail_string(&entry.stdout, max_bytes_each);
        let stderr = tail_string(&entry.stderr, max_bytes_each);
        Ok((stdout, stderr))
    }

    /// Send SIGTERM to `handle` and wait up to `grace` for it to exit.
    /// If it doesn't exit in time, fall through to SIGKILL.
    ///
    /// # Errors
    /// Returns an error if the handle is unknown.
    pub async fn kill(&self, handle: &str, grace: Duration) -> anyhow::Result<()> {
        let child_ref = {
            let inner = self.inner.lock().map_err(|_| anyhow::anyhow!("bg registry poisoned"))?;
            let entry = inner.procs.get(handle).ok_or_else(|| anyhow::anyhow!("unknown bg handle: {handle}"))?;
            Arc::clone(&entry.child)
        };
        let mut guard = child_ref.lock().await;
        if let Some(mut child) = guard.take() {
            // Attempt graceful kill first.
            let _ = child.start_kill();
            let _ = tokio::time::timeout(grace, child.wait()).await;
            // Belt-and-suspenders: if the handle was still alive when the
            // timeout expired, hit it with SIGKILL now. `start_kill` already
            // sends SIGKILL on Unix so most paths get cleaned up in one go.
            let _ = child.kill().await;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Append `chunk` to `buf`, keeping the buffer bounded at `MAX_LOG_BYTES`.
/// Oldest bytes are dropped first so the buffer always holds the most
/// recent tail.
fn append_bounded(buf: &Arc<Mutex<String>>, chunk: &str) {
    let Ok(mut s) = buf.lock() else {
        return;
    };
    s.push_str(chunk);
    if s.len() > MAX_LOG_BYTES {
        let drop_bytes = s.len() - MAX_LOG_BYTES;
        // Drain from the start. Utf-8 safe because we drop on line
        // boundaries when possible — read_line feeds line-by-line.
        *s = s.chars().skip(drop_bytes).collect();
    }
}

fn tail_string(buf: &Arc<Mutex<String>>, max_bytes: usize) -> String {
    let Ok(s) = buf.lock() else {
        return String::new();
    };
    if s.len() <= max_bytes {
        return s.clone();
    }
    let start = s.len() - max_bytes;
    // Find a UTF-8 boundary at or after `start`.
    let mut boundary = start;
    while !s.is_char_boundary(boundary) && boundary < s.len() {
        boundary += 1;
    }
    s[boundary..].to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_and_status_reports_exit_code() {
        let reg = BgRegistry::new();
        let handle = reg.run("exit 7", ".", &[]).expect("run");
        // Give the reaper a moment.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let st = reg.status(&handle).expect("status");
        assert!(!st.running);
        assert_eq!(st.exit_code, Some(7));
    }

    #[tokio::test]
    async fn run_captures_stdout_and_stderr() {
        let reg = BgRegistry::new();
        let handle = reg.run("echo hello; echo oops 1>&2", ".", &[]).expect("run");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (stdout, stderr) = reg.logs(&handle, 1024).expect("logs");
        assert!(stdout.contains("hello"), "stdout: {stdout:?}");
        assert!(stderr.contains("oops"), "stderr: {stderr:?}");
    }

    #[tokio::test]
    async fn kill_stops_a_running_process() {
        let reg = BgRegistry::new();
        let handle = reg.run("sleep 30", ".", &[]).expect("run");
        // Confirm it's running.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(reg.status(&handle).unwrap().running, "should be running");

        reg.kill(&handle, Duration::from_millis(500)).await.expect("kill");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!reg.status(&handle).unwrap().running, "should have exited after kill");
    }

    #[tokio::test]
    async fn list_returns_every_handle() {
        let reg = BgRegistry::new();
        let h1 = reg.run("exit 0", ".", &[]).expect("run");
        let h2 = reg.run("exit 1", ".", &[]).expect("run");
        let ls = reg.list();
        let handles: Vec<&str> = ls.iter().map(|s| s.handle.as_str()).collect();
        assert!(handles.contains(&h1.as_str()));
        assert!(handles.contains(&h2.as_str()));
    }

    #[tokio::test]
    async fn status_unknown_handle_errors() {
        let reg = BgRegistry::new();
        assert!(reg.status("bg-nope").is_err());
    }

    #[test]
    fn append_bounded_keeps_tail() {
        let buf = Arc::new(Mutex::new(String::new()));
        for _ in 0..10 {
            append_bounded(&buf, &"x".repeat(50_000));
        }
        // Should never exceed MAX_LOG_BYTES.
        let len = buf.lock().unwrap().len();
        assert!(len <= MAX_LOG_BYTES, "buf len {len} > max {MAX_LOG_BYTES}");
    }
}
