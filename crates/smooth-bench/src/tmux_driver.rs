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
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
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

/// Minimum number of non-whitespace bytes the captured pane must
/// contain before `wait_for_idle` will declare the TUI idle. Below
/// this floor we treat the pane as "still booting" and keep polling.
/// The `th code` TUI's steady-state frame (header + status line +
/// input prompt) is comfortably over 200 chars, so this floor is well
/// below the real signal but above an "empty pane" false-idle.
pub const DEFAULT_IDLE_MIN_BYTES: usize = 200;

/// Pane geometry — wide enough that wrap doesn't shred tool output.
/// 200x80 mirrors a typical large terminal; the TUI scales to it.
pub const PANE_WIDTH: u32 = 200;
pub const PANE_HEIGHT: u32 = 80;

/// Per-task debug sink. When set on a `TmuxDriver`, every `send` and
/// every `wait_for_idle` boundary appends a timestamped record to the
/// underlying writer. Created at the top of a task and dropped when
/// the driver is dropped — one file per (lang, task) pair.
///
/// Thread-safe so it can be cloned across the async task boundary.
#[derive(Clone)]
pub struct PaneDebugLog {
    inner: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
}

impl std::fmt::Debug for PaneDebugLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneDebugLog").finish_non_exhaustive()
    }
}

impl PaneDebugLog {
    /// Open a debug log at `path`, creating parent dirs. The writer
    /// is buffered — caller doesn't need to flush manually.
    ///
    /// # Errors
    /// Errors when the path can't be created/opened.
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        let f = std::fs::File::create(path).with_context(|| format!("create debug log {}", path.display()))?;
        let writer: Box<dyn std::io::Write + Send> = Box::new(std::io::BufWriter::new(f));
        Ok(Self {
            inner: Arc::new(Mutex::new(writer)),
        })
    }

    /// Append a labelled record. `label` is a short tag (e.g.
    /// `"send"`, `"idle"`, `"boot"`); `payload` is the pane snapshot
    /// or the text being sent. Errors are intentionally swallowed —
    /// debug logging must never crash a bench run.
    pub fn record(&self, label: &str, payload: &str) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let bytes = payload.len();
        let _ = writeln!(guard, "===== {ts} [{label}] bytes={bytes} =====");
        let _ = writeln!(guard, "{payload}");
        let _ = writeln!(guard);
    }
}

/// Drives a TUI process inside a detached tmux session. Owns the
/// session for its entire lifetime and tears it down on drop.
#[derive(Debug)]
pub struct TmuxDriver {
    session: String,
    /// Kept for diagnostic logging only; not used to address the pane.
    #[allow(dead_code)]
    workdir: PathBuf,
    /// Optional per-task debug log. Populated when the bench is run
    /// with `--debug`. Records every send + every idle/boot pane
    /// snapshot with timestamps.
    debug: Option<PaneDebugLog>,
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

    /// Attach (or replace) a debug log on this driver. Returns
    /// `self` for builder-style chaining at the call site.
    #[must_use]
    pub fn with_debug_log(mut self, debug: PaneDebugLog) -> Self {
        self.debug = Some(debug);
        self
    }

    /// Generic starter used both by the production `th code` path and
    /// by unit tests that drive `cat`, `echo`, etc. The command is
    /// run as a shell string so callers can chain (e.g. `cd … &&
    /// foo`); we always wrap it in `sh -c` for that reason.
    ///
    /// # Errors
    /// See [`start_th_code`].
    pub fn start_command(session: &str, workdir: &Path, shell_cmd: &str, boot_timeout: Duration) -> Result<Self> {
        Self::start_command_with_debug(session, workdir, shell_cmd, boot_timeout, None)
    }

    /// Same as [`start_command`] but with an optional `PaneDebugLog`
    /// attached BEFORE the first-render gate. Use this when you want
    /// the boot-screen captures recorded too — needed for diagnosing
    /// `th code` boot failures (pearl th-f46efa).
    ///
    /// # Errors
    /// See [`start_command`].
    pub fn start_command_with_debug(session: &str, workdir: &Path, shell_cmd: &str, boot_timeout: Duration, debug: Option<PaneDebugLog>) -> Result<Self> {
        require_tmux()?;

        // Reject existing sessions loudly — silently piggybacking onto
        // a stale session would corrupt the capture and drop another
        // run's session on our exit.
        if session_exists(session) {
            return Err(anyhow!(
                "tmux session `{session}` already exists; kill it (`tmux kill-session -t {session}`) or pick a different --tmux-session"
            ));
        }

        // `new-session -d -s NAME -x W -y H -c WORKDIR sh -c CMD`.
        // We capture stderr so an actual failure surfaces with a
        // useful message rather than being silently redirected — only
        // the "no server running" probe messages get swallowed (see
        // `session_exists` and `require_tmux`).
        let out = Command::new("tmux")
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
            .output()
            .context("spawning tmux new-session")?;
        if !out.status.success() {
            return Err(anyhow!(
                "tmux new-session for `{session}` exited non-zero: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        // After `new-session -d`, the server is up but the pane may
        // not yet exist for `capture-pane`. Poll `has-session` until
        // it reports the session is fully present (or boot_timeout).
        // This guards against a race where the very first `capture`
        // races the session creation. Without this, capture-pane can
        // return "can't find session" stderr noise — same family as
        // the "no server running" lines that triggered this pearl.
        let poll_start = Instant::now();
        loop {
            if session_exists(session) {
                break;
            }
            if poll_start.elapsed() > boot_timeout {
                return Err(anyhow!("tmux session `{session}` never showed up after new-session"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let driver = Self {
            session: session.to_string(),
            workdir: workdir.to_path_buf(),
            debug,
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
    /// Uses tmux's `load-buffer` + `paste-buffer` instead of
    /// `send-keys -l` because the latter interprets embedded `\n`
    /// bytes as the `C-j` keysym, and in literal-mode (`-l`) `C-j`
    /// degrades to the bare letter `j`. The result was the
    /// score-tui-pr regression where every newline in a multi-line
    /// task prompt rendered as a literal `j` in the TUI (pearl
    /// th-7fdfa9 debug log lines 1065/1151, etc.). `load-buffer`
    /// reads the payload as raw bytes from stdin and `paste-buffer`
    /// inserts it as terminal input the same way a real human paste
    /// would — newlines included. After pasting the buffer we send a
    /// separate `Enter` keystroke to submit, just as before.
    ///
    /// # Errors
    /// Errors if tmux send-keys / load-buffer / paste-buffer fails
    /// (e.g. session was killed).
    pub fn send(&self, text: &str) -> Result<()> {
        if let Some(dbg) = &self.debug {
            dbg.record("send", text);
        }

        // Use a uniquely named tmux buffer per send so concurrent
        // drivers don't trample each other's payloads. The buffer
        // name only has to be valid ASCII and distinct from any
        // other live buffer; we delete it immediately after pasting.
        let buffer_name = format!("smooth-bench-{}-{}", self.session, uuid::Uuid::new_v4().simple());

        // `load-buffer -b NAME -` reads raw bytes from stdin into the
        // named buffer. We feed `text` verbatim — no escaping needed.
        let mut child = Command::new("tmux")
            .args(["load-buffer", "-b", &buffer_name, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawning tmux load-buffer")?;
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().context("tmux load-buffer stdin missing")?;
            stdin.write_all(text.as_bytes()).context("writing payload to tmux load-buffer")?;
        }
        let out = child.wait_with_output().context("waiting on tmux load-buffer")?;
        if !out.status.success() {
            return Err(anyhow!("tmux load-buffer exited non-zero: {}", String::from_utf8_lossy(&out.stderr)));
        }

        // `paste-buffer -b NAME -t SESSION -d -p` inserts the buffer's
        // bytes into the pane as if pasted, then deletes the buffer.
        // The `-p` flag wraps the paste in bracketed-paste markers
        // (`\e[200~ ... \e[201~`) when the receiving application has
        // enabled bracketed-paste mode (`\e[?2004h`). Bracketed-paste-
        // aware TUIs use this to distinguish pasted content from typed
        // input — critically, they treat embedded newlines as soft
        // newlines (insert) rather than Enter (submit).
        //
        // Without `-p`, multi-line task prompts arrived in the
        // `smooth-code` TUI as N separate `You:` submissions, one per
        // newline, because the input handler treats each `\n` as
        // Enter. With `-p`, a bracketed-paste-aware TUI receives the
        // whole prompt as one submission. If the TUI hasn't enabled
        // bracketed-paste mode, tmux strips the `-p` markers and
        // behaviour is identical to the non-`-p` path — `-p` is safe
        // for non-aware applications.
        //
        // Belt-and-suspenders: the prompt is also constructed as a
        // single line in `build_prompt` so the multi-line interpretation
        // never arises regardless of the receiver's bracketed-paste
        // support. See pearl th-01c714.
        let out = Command::new("tmux")
            .args(["paste-buffer", "-b", &buffer_name, "-t", &self.session, "-d", "-p"])
            .output()
            .context("tmux paste-buffer")?;
        if !out.status.success() {
            // Best-effort buffer cleanup in case the paste failed
            // after load — otherwise the buffer leaks for the
            // session's lifetime.
            let _ = Command::new("tmux")
                .args(["delete-buffer", "-b", &buffer_name])
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .status();
            return Err(anyhow!("tmux paste-buffer exited non-zero: {}", String::from_utf8_lossy(&out.stderr)));
        }

        // Submit. Separate call so the Enter is interpreted as the
        // key, not as a pasted literal newline (which on most TUIs
        // would be equivalent anyway, but bracketed-paste-aware apps
        // could distinguish them). Matches prior behaviour where the
        // explicit Enter keystroke is what triggers submit.
        let out = Command::new("tmux")
            .args(["send-keys", "-t", &self.session, "Enter"])
            .output()
            .context("tmux send-keys (Enter)")?;
        if !out.status.success() {
            return Err(anyhow!("tmux send-keys (Enter) exited non-zero: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    }

    /// Capture the currently visible pane text (no escape codes).
    ///
    /// # Errors
    /// Errors only on tmux failure (e.g. the session was killed —
    /// usually because the child command exited). An empty pane
    /// returns `Ok("")`.
    pub fn capture(&self) -> Result<String> {
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &self.session, "-p"])
            .output()
            .context("tmux capture-pane")?;
        if !out.status.success() {
            // Include tmux's own stderr in the error — it tells you
            // *why* (e.g. "can't find session", "no server running").
            return Err(anyhow!(
                "tmux capture-pane exited non-zero (session `{}`): {}",
                self.session,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
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
        self.wait_for_idle_with_floor(dwell, poll_interval, overall_timeout, DEFAULT_IDLE_MIN_BYTES)
    }

    /// Same as [`wait_for_idle`] but with a configurable minimum
    /// non-whitespace byte count the pane must contain before being
    /// considered "idle". Below the floor, we treat the pane as still
    /// rendering / still booting and keep polling. This protects
    /// against the empty-pane false-idle that masked PR #55's broken
    /// runs (driver bailed in 38s because `capture-pane` returned
    /// stable empty output after the boot).
    ///
    /// `min_bytes` of 0 reproduces the original behaviour.
    ///
    /// # Errors
    /// As [`wait_for_idle`].
    pub fn wait_for_idle_with_floor(&self, dwell: Duration, poll_interval: Duration, overall_timeout: Duration, min_bytes: usize) -> Result<String> {
        let started = Instant::now();
        let mut last_text = self.capture()?;
        let mut stable_since = Instant::now();

        loop {
            std::thread::sleep(poll_interval);
            let now_text = self.capture()?;

            if now_text == last_text {
                let printable = now_text.chars().filter(|c| !c.is_whitespace()).count();
                if stable_since.elapsed() >= dwell && printable >= min_bytes {
                    if let Some(dbg) = &self.debug {
                        dbg.record("idle", &now_text);
                    }
                    return Ok(now_text);
                }
            } else {
                last_text = now_text;
                stable_since = Instant::now();
            }

            if started.elapsed() > overall_timeout {
                if let Some(dbg) = &self.debug {
                    dbg.record("idle_timeout", &last_text);
                }
                return Err(anyhow!(
                    "tmux pane did not settle within {overall_timeout:?} (dwell {dwell:?}, min_bytes {min_bytes}); last capture follows:\n{last_text}"
                ));
            }
        }
    }

    /// Read the live tmux session name. Useful for diagnostics.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Append a free-form labelled record to this driver's debug
    /// log, if one is attached. Used by higher-level harness code
    /// (e.g. the human-driver loop's slash-command guard) to leave
    /// a breadcrumb in the same pane.log a human would read.
    pub fn debug_record(&self, label: &str, payload: &str) {
        if let Some(dbg) = &self.debug {
            dbg.record(label, payload);
        }
    }

    /// Test helper: attach to an already-created tmux session
    /// without running the boot-render gate. Lets unit tests build a
    /// minimal `cat` session that wouldn't pass the first-render
    /// floor (since `cat` produces no output until typed at). Not
    /// intended for production code.
    ///
    /// # Errors
    /// Errors if the session doesn't exist.
    #[cfg(test)]
    pub fn attach_existing_for_test(session: &str, workdir: &Path) -> Result<Self> {
        if !session_exists(session) {
            return Err(anyhow!("tmux session `{session}` does not exist"));
        }
        Ok(Self {
            session: session.to_string(),
            workdir: workdir.to_path_buf(),
            debug: None,
        })
    }

    /// Block until the first non-trivial render, with timeout. The
    /// pane must contain at least `DEFAULT_IDLE_MIN_BYTES`
    /// non-whitespace characters — guards against "a single dot or
    /// cursor blink counts as rendered" false positives that were
    /// letting the bench race ahead of the TUI's actual boot.
    fn wait_for_first_render(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut last_text = String::new();
        loop {
            // Note: a capture failure most commonly means the child
            // command exited and tmux killed the session. Record the
            // failure + the last-known good capture so the debug log
            // tells the story (last frame the user saw before the
            // session went away). Then surface the error.
            let text = match self.capture() {
                Ok(t) => t,
                Err(e) => {
                    if let Some(dbg) = &self.debug {
                        dbg.record("capture_error", &format!("{e:#}\n(last good capture follows)\n{last_text}"));
                    }
                    return Err(e);
                }
            };
            let printable = text.chars().filter(|c| !c.is_whitespace()).count();
            if printable >= DEFAULT_IDLE_MIN_BYTES {
                if let Some(dbg) = &self.debug {
                    dbg.record("boot", &text);
                }
                return Ok(());
            }
            // Stream low-content captures into the debug log too —
            // otherwise a boot screen that animates without growing
            // past the floor leaves zero debug output and the op has
            // nothing to look at. Cap to one record per second by
            // recording only when the text changes.
            if let Some(dbg) = &self.debug {
                if text != last_text {
                    dbg.record("boot_partial", &text);
                }
            }
            last_text = text.clone();
            if started.elapsed() > timeout {
                if let Some(dbg) = &self.debug {
                    dbg.record("boot_timeout", &text);
                }
                return Err(anyhow!(
                    "tmux session `{}` never reached {} non-whitespace chars within {:?} — did the command exit immediately? Last capture follows:\n{}",
                    self.session,
                    DEFAULT_IDLE_MIN_BYTES,
                    timeout,
                    text
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for TmuxDriver {
    fn drop(&mut self) {
        // Best-effort cleanup: silently ignore errors AND silence
        // tmux's stderr — the session may already be gone if
        // start_command failed mid-way (e.g. `th code` crashed
        // during boot, the server is gone), and tmux prints "no
        // server running on …" to stderr in that case. Without the
        // Stdio::null redirect that line leaked into the bench
        // observer log — the original score-tui-pr.log showed two
        // copies per task because both this drop and a follow-on
        // probe each surfaced a copy.
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status();
    }
}

fn require_tmux() -> Result<()> {
    // Suppress stderr — `tmux -V` succeeds without a server, but on
    // some systems it still prints diagnostic noise we don't want
    // leaking into the bench observer output.
    let out = Command::new("tmux").arg("-V").stderr(Stdio::null()).output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(_) => Err(anyhow!("`tmux -V` exited non-zero — is tmux installed?")),
        Err(e) => Err(anyhow!("`tmux` not found on PATH ({e}); install tmux to use score-tui")),
    }
}

fn session_exists(session: &str) -> bool {
    // `tmux has-session -t NAME` returns 0 if present, 1 otherwise.
    // We can't trust stderr (tmux prints "no server running on …" or
    // "can't find session" to it, which is *normal* when probing
    // before any session has been created). Use the status code as
    // the source of truth and silence stderr so the user-facing
    // bench observer log stays clean.
    //
    // A missing tmux binary returns false here — `require_tmux`
    // surfaces that separately with a clearer message.
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
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

    /// Generate a payload of `n` repeated copies of `stem` joined by
    /// spaces — used by tests to clear the boot-floor + idle-floor
    /// thresholds without coupling to magic numbers.
    fn long_payload(stem: &str, n: usize) -> String {
        std::iter::repeat_n(stem, n).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn capture_returns_echoed_text() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("capture");

        // Emit ≥ DEFAULT_IDLE_MIN_BYTES non-whitespace chars so the
        // first-render gate fires. "hello-from-bench" repeated 30
        // times is comfortably over the 200-char floor.
        let payload = long_payload("hello-from-bench", 30);
        let cmd = format!("echo '{payload}' && sleep 60");
        let driver = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect("start tmux session");

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
        let payload = long_payload("initial-text", 30);
        let cmd = format!("echo '{payload}' && sleep 60");
        let driver = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect("start tmux session");

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
    fn wait_for_idle_with_floor_rejects_empty_pane() {
        // Regression for the score-tui-pr empty-pane false-idle bug:
        // a pane that is stable but holds < min_bytes printable chars
        // must NOT be declared idle. Without the floor, the bench
        // raced past `th code`'s actual boot and burned the run.
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("idle-floor");
        // `sleep 60` produces zero output — pane is stable & empty.
        // We can't call start_command (its boot gate would fail too)
        // so we construct manually.
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-x",
                &PANE_WIDTH.to_string(),
                "-y",
                &PANE_HEIGHT.to_string(),
                "-c",
                &tmp.path().to_string_lossy(),
                "sh",
                "-c",
                "sleep 60",
            ])
            .status()
            .expect("tmux new-session");
        assert!(status.success());
        let driver = TmuxDriver {
            session: session.clone(),
            workdir: tmp.path().to_path_buf(),
            debug: None,
        };

        // With min_bytes=200, an empty pane must time out — not be
        // mistaken for idle.
        let err = driver
            .wait_for_idle_with_floor(Duration::from_millis(400), Duration::from_millis(100), Duration::from_millis(1_200), 200)
            .expect_err("empty pane must not be treated as idle");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not settle"), "expected timeout error; got: {msg}");

        // And with min_bytes=0 the old behaviour returns idle.
        let text = driver
            .wait_for_idle_with_floor(Duration::from_millis(400), Duration::from_millis(100), Duration::from_secs(2), 0)
            .expect("empty pane treated as idle with no floor");
        assert!(text.trim().is_empty(), "expected empty pane; got: {text:?}");
    }

    #[test]
    fn debug_log_records_send_and_idle() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("debug-log");
        let payload = long_payload("boot-payload", 30);
        let cmd = format!("echo '{payload}' && cat");
        let driver = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect("start");

        let log_path = tmp.path().join("debug.log");
        let dbg = PaneDebugLog::create(&log_path).expect("create debug log");
        let driver = driver.with_debug_log(dbg);

        // Idle should record the boot pane snapshot.
        let _ = driver
            .wait_for_idle(Duration::from_millis(400), Duration::from_millis(100), Duration::from_secs(3))
            .expect("idle settles");
        driver.send("hi-debug").expect("send");

        // Drop the driver so the BufWriter inside the log gets
        // flushed on Drop.
        drop(driver);

        let logged = std::fs::read_to_string(&log_path).expect("read debug log");
        assert!(logged.contains("[idle]"), "expected idle record; got:\n{logged}");
        assert!(logged.contains("[send]"), "expected send record; got:\n{logged}");
        assert!(logged.contains("hi-debug"), "expected sent payload in log; got:\n{logged}");
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
                    debug: None,
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
    fn send_preserves_newlines_no_j_leakage() {
        // Regression for pearl th-7fdfa9: `send-keys -l` interpreted
        // every `\n` in the payload as the `C-j` keysym, which in
        // literal-mode degrades to the bare letter `j`. Multi-line
        // task prompts ended up with `j` characters where the
        // newlines should have been. The fix switches `send` to
        // `load-buffer` + `paste-buffer`. This test asserts that a
        // 3-line message lands as 3 lines in a file written by `cat
        // > tmpfile` — no stray `j`s anywhere.
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let outfile = tmp.path().join("captured.txt");
        let session = unique_session("newlines");
        // `cat > FILE` writes typed input to the file until EOF /
        // session kill. We want the file to contain exactly what we
        // sent. Use `sh -c` so the redirection takes effect.
        let cmd = format!("cat > {}", outfile.display());

        // `cat` produces no first-render output, so start_command's
        // boot gate would time out. Build the driver manually.
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-x",
                &PANE_WIDTH.to_string(),
                "-y",
                &PANE_HEIGHT.to_string(),
                "-c",
                &tmp.path().to_string_lossy(),
                "sh",
                "-c",
                &cmd,
            ])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        let driver = TmuxDriver {
            session: session.clone(),
            workdir: tmp.path().to_path_buf(),
            debug: None,
        };

        // Send a 3-line message. The Enter after the buffer paste
        // adds a 4th newline; that's intentional and matches what a
        // human pressing Enter at the end of a multi-line paste
        // would produce. We assert on the first 3 lines.
        let payload = "line-one\nline-two\nline-three";
        driver.send(payload).expect("send multi-line");

        // Give `cat` a moment to flush its input into the file. Then
        // kill the session so `cat` sees EOF and exits cleanly.
        std::thread::sleep(Duration::from_millis(500));
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status();
        // Give the FS a beat to settle the redirect target.
        std::thread::sleep(Duration::from_millis(150));

        let written = std::fs::read_to_string(&outfile).expect("read captured file");
        // No stray `j`s — the regression's signature.
        assert!(!written.contains('j'), "captured file contains a stray `j` (newline regression):\n{written}");
        // Real newlines preserved.
        let lines: Vec<&str> = written.lines().collect();
        assert!(
            lines.len() >= 3,
            "expected at least 3 lines from 3-line payload + Enter; got {}:\n{written}",
            lines.len()
        );
        assert_eq!(lines[0], "line-one", "line 1 mismatch in:\n{written}");
        assert_eq!(lines[1], "line-two", "line 2 mismatch in:\n{written}");
        assert_eq!(lines[2], "line-three", "line 3 mismatch in:\n{written}");
        // Driver's Drop best-effort kills the already-dead session;
        // that's fine, errors are swallowed there.
        drop(driver);
    }

    #[test]
    fn duplicate_session_name_is_rejected() {
        if !tmux_present() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let session = unique_session("dup");
        let payload = long_payload("first-aaaaa", 30);
        let cmd = format!("echo '{payload}' && sleep 30");
        let _driver = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect("first start");

        let err = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect_err("second start must fail");
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
        let payload = long_payload("alive-aaaaa", 30);
        let cmd = format!("echo '{payload}' && sleep 60");
        {
            let _driver = TmuxDriver::start_command(&session, tmp.path(), &cmd, Duration::from_secs(5)).expect("start");
            assert!(session_exists(&session), "session should exist while driver alive");
        }
        // Drop ran — session must be gone.
        assert!(!session_exists(&session), "session must be killed when driver drops");
    }
}
