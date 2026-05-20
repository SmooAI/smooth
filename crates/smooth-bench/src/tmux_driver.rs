//! tmux-backed driver for `score-tui` — drives a child TUI through a
//! detached tmux session so the bench can exercise the same surface a
//! human user touches (rendered output, keystroke input, model display
//! in the alias→upstream format, tool-call surfacing, session
//! lifecycle).
//!
//! Why tmux (vs. PTY-direct via portable-pty / pty-process):
//! - The TUI assumes a real terminal: alt-screen, cursor control,
//!   resize handling, full color. tmux gives us all of that for free
//!   without re-implementing a terminal emulator.
//! - `tmux capture-pane -p` returns the already-rendered visible text,
//!   which is what a human would see — perfect for the LLM-as-human
//!   loop. A raw PTY stream would need ANSI parsing on our side.
//! - tmux is already an assumed dev dep on this repo's machines.
//!
//! Lifecycle:
//! - `TmuxDriver::start_th_code` creates a detached session, runs the
//!   target command, polls `capture-pane` until the first render
//!   stabilises, and returns the live driver.
//! - `send` types into the pane via `send-keys`, ending with Enter.
//! - `capture` returns the current visible pane text.
//! - `wait_for_idle` polls `capture` every ~500ms and returns when the
//!   text has been stable for ≥ `idle_dwell` (default 2s) — our
//!   heuristic for "the TUI is done reacting and the user can speak
//!   again". Documented in module docs below in case the dwell needs
//!   tuning.
//! - `Drop` kills the session so a failed bench run doesn't leak.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// How long the pane text must stay byte-identical before we call the
/// TUI "idle". 2s is the documented default in CLAUDE.md for this
/// pearl; raise via `SMOOTH_BENCH_TUI_IDLE_DWELL_MS` if the agent
/// pauses to think without printing for longer.
pub const DEFAULT_IDLE_DWELL: Duration = Duration::from_millis(2_000);

/// How often `wait_for_idle` re-samples the pane. 500ms is responsive
/// enough that a ~2s dwell window catches the first quiet moment
/// without burning a CPU core on capture-pane.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Pane geometry — wide enough that wrap doesn't shred tool output.
/// 200x80 mirrors a typical large terminal; the TUI scales to it.
pub const PANE_WIDTH: u32 = 200;
pub const PANE_HEIGHT: u32 = 80;

/// Drives a TUI process inside a detached tmux session. Owns the
/// session for its entire lifetime and tears it down on drop.
#[derive(Debug)]
pub struct TmuxDriver {
    session: String,
    /// Kept for diagnostic logging only; not used to address the pane.
    #[allow(dead_code)]
    workdir: PathBuf,
}

impl TmuxDriver {
    /// Spawn `th code` inside a fresh tmux session rooted at
    /// `workdir`. Polls `capture-pane` until the visible text
    /// stabilises (initial render finished) and then returns the
    /// driver.
    ///
    /// # Errors
    /// Returns an error when:
    /// - `tmux` is not on PATH (clear "install tmux" message).
    /// - A session with the same name already exists.
    /// - The TUI never renders anything within `boot_timeout`.
    pub fn start_th_code(session: &str, workdir: &Path, boot_timeout: Duration) -> Result<Self> {
        Self::start_command(session, workdir, "th code", boot_timeout)
    }

    /// Generic starter used both by the production `th code` path and
    /// by unit tests that drive `cat`, `echo`, etc. The command is
    /// run as a shell string so callers can chain (e.g. `cd … &&
    /// foo`); we always wrap it in `sh -c` for that reason.
    ///
    /// # Errors
    /// See [`start_th_code`].
    pub fn start_command(session: &str, workdir: &Path, shell_cmd: &str, boot_timeout: Duration) -> Result<Self> {
        require_tmux()?;

        // Reject existing sessions loudly — silently piggybacking onto
        // a stale session would corrupt the capture and drop another
        // run's session on our exit.
        if session_exists(session) {
            return Err(anyhow!(
                "tmux session `{session}` already exists; kill it (`tmux kill-session -t {session}`) or pick a different --tmux-session"
            ));
        }

        // `new-session -d -s NAME -x W -y H -c WORKDIR sh -c CMD`
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                &PANE_WIDTH.to_string(),
                "-y",
                &PANE_HEIGHT.to_string(),
                "-c",
                &workdir.to_string_lossy(),
                "sh",
                "-c",
                shell_cmd,
            ])
            .status()
            .context("spawning tmux new-session")?;
        if !status.success() {
            return Err(anyhow!("tmux new-session for `{session}` exited non-zero"));
        }

        let driver = Self {
            session: session.to_string(),
            workdir: workdir.to_path_buf(),
        };

        // Wait for the TUI to render at least one non-empty frame
        // before we hand the driver back. This catches "tmux session
        // up but the command died immediately" by timing out below.
        driver.wait_for_first_render(boot_timeout)?;

        Ok(driver)
    }

    /// Type `text` into the pane followed by `Enter`. Returns
    /// immediately — does not wait for the TUI to react. Callers who
    /// need to wait should follow with [`wait_for_idle`].
    ///
    /// # Errors
    /// Errors if tmux send-keys fails (e.g. session was killed).
    pub fn send(&self, text: &str) -> Result<()> {
        // Use `-l` (literal) so backticks / dollar signs / quotes in
        // the LLM-generated message are typed verbatim rather than
        // interpreted by tmux's key parser.
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.session, "-l", text])
            .status()
            .context("tmux send-keys (literal payload)")?;
        if !status.success() {
            return Err(anyhow!("tmux send-keys (payload) exited non-zero"));
        }
        // Submit. Separate call so `-l` only applies to the payload —
        // we DO want `Enter` to be interpreted as the key, not the
        // literal characters "Enter".
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.session, "Enter"])
            .status()
            .context("tmux send-keys (Enter)")?;
        if !status.success() {
            return Err(anyhow!("tmux send-keys (Enter) exited non-zero"));
        }
        Ok(())
    }

    /// Capture the currently visible pane text (no escape codes).
    ///
    /// # Errors
    /// Errors only on tmux failure; an empty pane returns `Ok("")`.
    pub fn capture(&self) -> Result<String> {
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &self.session, "-p"])
            .output()
            .context("tmux capture-pane")?;
        if !out.status.success() {
            return Err(anyhow!("tmux capture-pane exited non-zero"));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Poll the pane every `poll_interval` and return when the text
    /// has been byte-identical for `dwell` consecutive samples (i.e.
    /// the TUI has been visibly quiet for at least `dwell`). Errors
    /// out after `overall_timeout` regardless.
    ///
    /// Returns the final captured pane text on success.
    ///
    /// Heuristic notes:
    /// - 2s dwell catches "agent finished printing, awaiting input"
    ///   reliably for the current `th code` shape. If the agent
    ///   pauses to think mid-output for >2s, this can mis-fire — bump
    ///   `dwell` to 5s via env override when that matters.
    /// - Falsely declaring idle is preferable to falsely declaring
    ///   busy: the LLM-as-human loop will recover by asking
    ///   "anything else?" on the next turn and waiting again.
    ///
    /// # Errors
    /// Errors on tmux failure, or if the pane never settles within
    /// `overall_timeout`.
    pub fn wait_for_idle(&self, dwell: Duration, poll_interval: Duration, overall_timeout: Duration) -> Result<String> {
        let started = Instant::now();
        let mut last_text = self.capture()?;
        let mut stable_since = Instant::now();

        loop {
            std::thread::sleep(poll_interval);
            let now_text = self.capture()?;

            if now_text == last_text {
                if stable_since.elapsed() >= dwell {
                    return Ok(now_text);
                }
            } else {
                last_text = now_text;
                stable_since = Instant::now();
            }

            if started.elapsed() > overall_timeout {
                return Err(anyhow!(
                    "tmux pane did not settle within {overall_timeout:?} (dwell {dwell:?}); last capture follows:\n{last_text}"
                ));
            }
        }
    }

    /// Read the live tmux session name. Useful for diagnostics.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Block until the very first non-empty render, with timeout.
    /// "Non-empty" = at least one printable character in the capture.
    fn wait_for_first_render(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        loop {
            let text = self.capture()?;
            if text.chars().any(|c| !c.is_whitespace()) {
                return Ok(());
            }
            if started.elapsed() > timeout {
                return Err(anyhow!(
                    "tmux session `{}` never produced visible output within {:?} — did the command exit immediately?",
                    self.session,
                    timeout
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for TmuxDriver {
    fn drop(&mut self) {
        // Best-effort cleanup: silently ignore errors — the session
        // may already be gone if start_command failed mid-way.
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.session]).status();
    }
}

fn require_tmux() -> Result<()> {
    let out = Command::new("tmux").arg("-V").output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(_) => Err(anyhow!("`tmux -V` exited non-zero — is tmux installed?")),
        Err(e) => Err(anyhow!("`tmux` not found on PATH ({e}); install tmux to use score-tui")),
    }
}

fn session_exists(session: &str) -> bool {
    // `tmux has-session -t NAME` returns 0 if present, 1 otherwise.
    // We can't trust stderr (tmux prints "can't find session" to it,
    // which is normal); use the status code as the source of truth.
    // A missing tmux binary returns false here — `require_tmux`
    // surfaces that separately with a clearer message.
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique session name per test so parallel runs don't collide.
    fn unique_session(stem: &str) -> String {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        format!("smooth-bench-test-{stem}-{pid}-{n}")
    }

    fn tmux_present() -> bool {
        Command::new("tmux").arg("-V").output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn capture_returns_echoed_text() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("capture");

        // `echo hello && sleep 60` so the session stays alive long
        // enough for us to capture it. tmux closes the pane when the
        // command exits.
        let driver = TmuxDriver::start_command(&session, tmp.path(), "echo hello-from-bench && sleep 60", Duration::from_secs(5)).expect("start tmux session");

        // The echo'd text should be visible.
        let text = driver.capture().expect("capture");
        assert!(text.contains("hello-from-bench"), "expected captured text to include payload; got:\n{text}");
    }

    #[test]
    fn wait_for_idle_returns_after_dwell() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("idle");
        let driver = TmuxDriver::start_command(&session, tmp.path(), "echo initial-text && sleep 60", Duration::from_secs(5)).expect("start tmux session");

        // After echo, the pane is quiescent — wait_for_idle should
        // return promptly once dwell has elapsed.
        let started = Instant::now();
        let final_text = driver
            .wait_for_idle(Duration::from_millis(500), Duration::from_millis(150), Duration::from_secs(5))
            .expect("idle settles");
        let elapsed = started.elapsed();
        assert!(
            final_text.contains("initial-text"),
            "expected idle capture to include initial echo; got:\n{final_text}"
        );
        // Dwell is 500ms; we expect somewhere in the 500ms-2s range.
        assert!(
            elapsed >= Duration::from_millis(500),
            "wait_for_idle returned before dwell elapsed ({elapsed:?})"
        );
    }

    #[test]
    fn send_writes_text_to_pane() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("send");
        // `cat` echoes whatever the driver types back to the pane.
        let driver = TmuxDriver::start_command(&session, tmp.path(), "cat", Duration::from_secs(5));
        // `cat` produces no first-render output, so start_command may
        // time out on wait_for_first_render. Tolerate that and still
        // exercise the send path.
        let driver = match driver {
            Ok(d) => d,
            Err(_) => {
                // Recreate the session manually, skipping the
                // first-render gate. We still get a working driver.
                let session2 = unique_session("send2");
                let status = Command::new("tmux")
                    .args([
                        "new-session",
                        "-d",
                        "-s",
                        &session2,
                        "-x",
                        &PANE_WIDTH.to_string(),
                        "-y",
                        &PANE_HEIGHT.to_string(),
                        "-c",
                        &tmp.path().to_string_lossy(),
                        "sh",
                        "-c",
                        "cat",
                    ])
                    .status()
                    .expect("tmux new-session");
                assert!(status.success());
                TmuxDriver {
                    session: session2,
                    workdir: tmp.path().to_path_buf(),
                }
            }
        };

        driver.send("hello-echo-back").expect("send");
        // Give cat a moment to echo before we capture.
        std::thread::sleep(Duration::from_millis(500));
        let text = driver.capture().expect("capture");
        assert!(text.contains("hello-echo-back"), "expected pane to contain typed text; got:\n{text}");
    }

    #[test]
    fn duplicate_session_name_is_rejected() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("dup");
        let _driver = TmuxDriver::start_command(&session, tmp.path(), "echo first && sleep 30", Duration::from_secs(5)).expect("first start");

        let err = TmuxDriver::start_command(&session, tmp.path(), "echo second && sleep 30", Duration::from_secs(5)).expect_err("second start must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("already exists"), "expected duplicate-session error; got: {msg}");
    }

    #[test]
    fn drop_kills_session() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("dropkill");
        {
            let _driver = TmuxDriver::start_command(&session, tmp.path(), "echo alive && sleep 60", Duration::from_secs(5)).expect("start");
            assert!(session_exists(&session), "session should exist while driver alive");
        }
        // Drop ran — session must be gone.
        assert!(!session_exists(&session), "session must be killed when driver drops");
    }
}
