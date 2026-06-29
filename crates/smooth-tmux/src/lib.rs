//! `smooth-tmux` — a small, generic driver for running and steering
//! programs inside isolated tmux sessions.
//!
//! This crate carries the hard-won tmux glue from the bench harness
//! (`smooth-bench`) in a dependency-light form so the shipped `th`
//! binary can drive interactive TUIs (notably Claude Code) without
//! pulling the heavy benchmark dependency tree.
//!
//! Design notes baked in from prior pain:
//! - **Per-driver socket isolation** (`tmux -L <socket>`): every driver
//!   gets a fresh tmux server so one driver's `Drop` can never tear down
//!   another's session, and a stale session can never be inherited.
//! - **Bracketed-paste send**: multi-line payloads are pasted as one
//!   submission rather than N newline-split submissions.
//! - **Scrollback capture** (`-S - -J`): the full history is captured,
//!   not just the visible region, so a supervisor can see content that
//!   scrolled off the top.
//!
//! The pure, IO-free helpers (`make_socket_name`, the `*_args` builders,
//! `truncate_from_front`, `samples_are_stable`) are public so they can be
//! unit-tested without a live tmux.

// `wait_for_idle` folds Duration-millis (u128) into a small usize sample
// count; the truncation is intentional and bounded.
#![allow(clippy::cast_possible_truncation)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// Default pane geometry. Wide enough that Claude Code's status line and
/// boxes render without wrapping artifacts that confuse pane scraping.
pub const PANE_WIDTH: u16 = 200;
/// Default pane height.
pub const PANE_HEIGHT: u16 = 50;

/// Default front-truncation budget for [`TmuxDriver::capture`]. A chatty
/// session can produce tens of KiB; we keep the most recent bytes.
pub const DEFAULT_CAPTURE_MAX_BYTES: usize = 64 * 1024;

/// A handle to a program running inside its own isolated tmux session.
///
/// Dropping the driver kills the session (best effort) so callers don't
/// leak tmux servers.
pub struct TmuxDriver {
    socket: String,
    session: String,
    capture_max_bytes: usize,
}

/// Build the tmux socket name for a driver keyed on `session`.
///
/// The name is made unique per process and per nanosecond so concurrent
/// drivers — and retries within one process — never collide. Non
/// alphanumeric characters in `session` are folded to `-`, and the
/// session-derived prefix is truncated so the final socket path stays
/// well under macOS's 104-byte `sun_path` limit.
#[must_use]
pub fn make_socket_name(session: &str) -> String {
    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    let cleaned: String = session.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let stub: String = cleaned.chars().take(28).collect();
    format!("smth-{stub}-{pid}-{ns}")
}

/// Arguments for `tmux new-session -d` in an isolated server.
///
/// The `-L <socket>` flag MUST come first — tmux treats it as a session
/// argument otherwise.
#[must_use]
pub fn new_session_args(socket: &str, session: &str, width: u16, height: u16, workdir: &str, shell_cmd: &str) -> Vec<String> {
    vec![
        "-L".into(),
        socket.into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.into(),
        "-x".into(),
        width.to_string(),
        "-y".into(),
        height.to_string(),
        "-c".into(),
        workdir.into(),
        "sh".into(),
        "-c".into(),
        shell_cmd.into(),
    ]
}

/// Arguments for `tmux capture-pane`.
///
/// With `scrollback`, capture the full history (`-S -`) and join wrapped
/// lines (`-J`); without it, capture only the visible region (cheaper;
/// good for scraping the status line).
#[must_use]
pub fn capture_args(socket: &str, session: &str, scrollback: bool) -> Vec<String> {
    let mut args = vec!["-L".into(), socket.into(), "capture-pane".into(), "-t".into(), session.into(), "-p".into()];
    if scrollback {
        args.push("-S".into());
        args.push("-".into());
        args.push("-J".into());
    }
    args
}

/// Truncate `s` from the FRONT (oldest content) so it fits in `max`
/// bytes, prepending a marker when truncation occurred. Recent content
/// is the most valuable to a supervisor, so we keep the tail.
#[must_use]
pub fn truncate_from_front(s: &str, max: usize) -> String {
    const MARKER: &str = "…[truncated]…\n";
    if s.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(MARKER.len());
    // Find a char boundary at or after `s.len() - keep`.
    let mut start = s.len() - keep;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("{MARKER}{}", &s[start..])
}

/// True when the last `dwell` samples are all byte-identical and
/// non-empty — i.e. the pane has stopped changing. `samples` is ordered
/// oldest→newest; only the last `dwell` are considered.
#[must_use]
pub fn samples_are_stable(samples: &[String], dwell: usize) -> bool {
    if dwell == 0 || samples.len() < dwell {
        return false;
    }
    let tail = &samples[samples.len() - dwell..];
    let first = &tail[0];
    !first.is_empty() && tail.iter().all(|s| s == first)
}

impl TmuxDriver {
    /// Spawn `shell_cmd` inside a fresh, isolated tmux session rooted at
    /// `workdir`, then wait until the pane first renders.
    ///
    /// # Errors
    /// - `tmux` is not on `PATH`.
    /// - The session could not be created.
    /// - The pane never rendered within `boot_timeout`.
    pub fn start(session: &str, workdir: &Path, shell_cmd: &str, boot_timeout: Duration) -> Result<Self> {
        require_tmux()?;
        let socket = make_socket_name(session);

        let args = new_session_args(&socket, session, PANE_WIDTH, PANE_HEIGHT, &workdir.to_string_lossy(), shell_cmd);
        let out = Command::new("tmux").args(&args).output().context("spawning tmux new-session")?;
        if !out.status.success() {
            return Err(anyhow!(
                "tmux new-session for `{session}` exited non-zero: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        let driver = Self {
            socket,
            session: session.to_string(),
            capture_max_bytes: DEFAULT_CAPTURE_MAX_BYTES,
        };

        // Gate on the session being fully present (not on rendered
        // content): right after `new-session -d` the pane may not yet
        // exist for `capture-pane`, but a program that renders nothing
        // until it receives input (e.g. `cat`) would never satisfy a
        // "non-empty render" gate. "Session is up and a capture
        // succeeds" is the right minimal guarantee that subsequent
        // send/capture won't race creation. Waiting for a specific first
        // render is the caller's job (`wait_for_idle` or poll for a
        // marker).
        let deadline = Instant::now() + boot_timeout;
        loop {
            if driver.is_alive() && driver.capture_visible().is_ok() {
                return Ok(driver);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("tmux session `{session}` never came up within {boot_timeout:?}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The tmux socket this driver owns.
    #[must_use]
    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// The tmux session name.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Override the front-truncation budget for [`capture`](Self::capture).
    pub fn set_capture_max_bytes(&mut self, n: usize) {
        self.capture_max_bytes = n;
    }

    /// Submit `text` to the pane as one bracketed-paste, followed by an
    /// explicit `Enter`. A unique tmux buffer is used per send so
    /// concurrent drivers never trample each other's payloads.
    ///
    /// # Errors
    /// On any underlying tmux command failure.
    pub fn send(&self, text: &str) -> Result<()> {
        let buffer = format!("smth-{}-{}", self.session, uuid::Uuid::new_v4().simple());

        let mut child = Command::new("tmux")
            .args(["-L", &self.socket, "load-buffer", "-b", &buffer, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
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
            return Err(anyhow!("tmux load-buffer exited non-zero: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }

        // `-p` wraps the paste in bracketed-paste markers so a TUI treats
        // embedded newlines as soft newlines, not Enter. `-d` deletes the
        // buffer afterward.
        let out = Command::new("tmux")
            .args(["-L", &self.socket, "paste-buffer", "-b", &buffer, "-t", &self.session, "-d", "-p"])
            .output()
            .context("tmux paste-buffer")?;
        if !out.status.success() {
            let _ = Command::new("tmux")
                .args(["-L", &self.socket, "delete-buffer", "-b", &buffer])
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .status();
            return Err(anyhow!("tmux paste-buffer exited non-zero: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }

        self.send_enter()
    }

    /// Send a bare `Enter` keystroke (submit the current input).
    ///
    /// # Errors
    /// On tmux failure.
    pub fn send_enter(&self) -> Result<()> {
        let out = Command::new("tmux")
            .args(["-L", &self.socket, "send-keys", "-t", &self.session, "Enter"])
            .output()
            .context("tmux send-keys (Enter)")?;
        if !out.status.success() {
            return Err(anyhow!(
                "tmux send-keys (Enter) exited non-zero: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Send a named key (e.g. `Escape`, `C-c`) to the pane.
    ///
    /// # Errors
    /// On tmux failure.
    pub fn send_key(&self, key: &str) -> Result<()> {
        let out = Command::new("tmux")
            .args(["-L", &self.socket, "send-keys", "-t", &self.session, key])
            .output()
            .context("tmux send-keys")?;
        if !out.status.success() {
            return Err(anyhow!(
                "tmux send-keys `{key}` exited non-zero: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Capture the pane including full scrollback, front-truncated to the
    /// configured byte budget.
    ///
    /// # Errors
    /// On tmux failure (e.g. the session was killed because the child
    /// exited). An empty pane returns `Ok("")`.
    pub fn capture(&self) -> Result<String> {
        let raw = self.capture_raw(true)?;
        Ok(truncate_from_front(&raw, self.capture_max_bytes))
    }

    /// Capture only the currently visible pane (no scrollback).
    ///
    /// # Errors
    /// On tmux failure.
    pub fn capture_visible(&self) -> Result<String> {
        self.capture_raw(false)
    }

    fn capture_raw(&self, scrollback: bool) -> Result<String> {
        let args = capture_args(&self.socket, &self.session, scrollback);
        let out = Command::new("tmux").args(&args).output().context("tmux capture-pane")?;
        if !out.status.success() {
            return Err(anyhow!(
                "tmux capture-pane exited non-zero (session `{}`): {}",
                self.session,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// True while the tmux session is still alive. Once the child program
    /// exits, tmux tears the session down and this returns `false`.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        Command::new("tmux")
            .args(["-L", &self.socket, "has-session", "-t", &self.session])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Poll the pane every `poll_interval` and return its captured text
    /// once it has been byte-identical for `dwell`. Errors after
    /// `overall_timeout` regardless.
    ///
    /// # Errors
    /// On tmux failure, or if the pane never settles in time.
    pub fn wait_for_idle(&self, dwell: Duration, poll_interval: Duration, overall_timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + overall_timeout;
        let dwell_samples = (dwell.as_millis() / poll_interval.as_millis().max(1)).max(1) as usize + 1;
        let mut samples: Vec<String> = Vec::new();
        loop {
            samples.push(self.capture()?);
            if samples_are_stable(&samples, dwell_samples) {
                return Ok(samples.pop().unwrap_or_default());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("pane for session `{}` never settled within {overall_timeout:?}", self.session));
            }
            std::thread::sleep(poll_interval);
        }
    }

    /// Kill this driver's tmux session and server.
    ///
    /// # Errors
    /// On tmux failure. Killing an already-dead session is not an error.
    pub fn kill(&self) -> Result<()> {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }
}

impl Drop for TmuxDriver {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

fn require_tmux() -> Result<()> {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| anyhow!("tmux is required but could not be run ({e}); install it (macOS: `brew install tmux`)"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(anyhow!("`tmux -V` failed; is tmux installed and on PATH?"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_name_is_unique_and_clean() {
        let a = make_socket_name("SMOODEV-1/weird name");
        let b = make_socket_name("SMOODEV-1/weird name");
        assert_ne!(a, b, "two calls must differ (nanosecond clock)");
        assert!(a.starts_with("smth-"));
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "no socket-hostile chars: {a}");
    }

    #[test]
    fn socket_name_truncates_long_session() {
        let long = "x".repeat(200);
        let name = make_socket_name(&long);
        // Prefix stub is capped at 28 chars; total path must stay short.
        assert!(name.len() < 80, "socket name too long: {} ({} bytes)", name, name.len());
    }

    #[test]
    fn new_session_args_put_socket_first() {
        let args = new_session_args("sock", "sess", 100, 40, "/tmp", "claude");
        assert_eq!(args[0], "-L");
        assert_eq!(args[1], "sock");
        assert_eq!(args[2], "new-session");
        assert!(args.contains(&"claude".to_string()));
        // width/height rendered as strings in order.
        let xi = args.iter().position(|a| a == "-x").unwrap();
        assert_eq!(args[xi + 1], "100");
    }

    #[test]
    fn capture_args_scrollback_toggle() {
        let with = capture_args("s", "sess", true);
        assert!(with.contains(&"-S".to_string()) && with.contains(&"-J".to_string()));
        let without = capture_args("s", "sess", false);
        assert!(!without.contains(&"-S".to_string()));
        assert!(without.contains(&"-p".to_string()));
    }

    #[test]
    fn truncate_keeps_tail_and_marks() {
        let s = "abcdefghij".repeat(10); // 100 bytes
        let out = truncate_from_front(&s, 40);
        assert!(out.starts_with("…[truncated]…"));
        assert!(out.ends_with("abcdefghij"), "tail preserved: {out}");
        assert!(out.len() <= 40 + "…[truncated]…\n".len());
    }

    #[test]
    fn truncate_noop_when_small() {
        assert_eq!(truncate_from_front("hi", 100), "hi");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "αβγδε".repeat(20); // multibyte
        let out = truncate_from_front(&s, 30);
        // Must be valid UTF-8 (no panic on slice) and start with marker.
        assert!(out.starts_with("…[truncated]…"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn stability_needs_full_dwell() {
        let s = |x: &str| x.to_string();
        assert!(!samples_are_stable(&[s("a")], 2));
        assert!(!samples_are_stable(&[s("a"), s("b")], 2));
        assert!(samples_are_stable(&[s("b"), s("a"), s("a")], 2));
        assert!(!samples_are_stable(&[s("a"), s("a")], 0), "dwell 0 is never stable");
    }

    #[test]
    fn stability_ignores_empty_panes() {
        let s = |x: &str| x.to_string();
        assert!(!samples_are_stable(&[s(""), s("")], 2), "blank panes are not 'idle'");
    }

    /// Live tmux smoke test. Skips (does not fail) when tmux is absent so
    /// CI runners without tmux stay green.
    #[test]
    fn live_roundtrip_when_tmux_available() {
        if require_tmux().is_err() {
            eprintln!("skipping: tmux not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // `echo READY; cat` gives a deterministic first render and then
        // echoes whatever we paste, so we can prove send+capture.
        let driver = TmuxDriver::start("smth-test-roundtrip", dir.path(), "echo READY; cat", Duration::from_secs(5)).unwrap();
        assert!(driver.is_alive());
        driver.send("hello-tmux-smoke").unwrap();
        // Give cat a moment to echo.
        let settled = driver
            .wait_for_idle(Duration::from_millis(400), Duration::from_millis(100), Duration::from_secs(5))
            .unwrap_or_default();
        assert!(settled.contains("hello-tmux-smoke"), "echoed payload not seen; pane=\n{settled}");
    }
}
