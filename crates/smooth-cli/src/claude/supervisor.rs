//! The 1:1 supervisor loop: launch a Claude Code TUI in an isolated tmux
//! session, send the initial prompt, and watch the pane until it exits or
//! the account hits its usage/quota limit.
//!
//! The transient server throttle is *not* handled here — Claude Code
//! retries it internally, so a supervisor-side backoff-and-resend only
//! risked double-sending a prompt on top of a recovering model.
//!
//! This is the degenerate case of every topology — one supervisor, one
//! session. The 1:N farm reuses the same pieces across N supervisors.
//!
//! The blocking loop touches tmux, so it is exercised by the live smoke
//! test; the pure decision (`action_for`) and helpers are unit tested
//! without tmux.

// `short_id` deliberately folds the nanosecond clock into a u64 for a
// short, throwaway id; the u128→u64 truncation is the intent.
#![allow(clippy::cast_possible_truncation)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anstream::println;
use anyhow::Result;
use chrono::Utc;
use owo_colors::OwoColorize;
use smooth_tmux::TmuxDriver;

use super::control;
use super::detect::{detect_state, PaneState};
use super::registry::{self, SessionEntry};

/// Options for one supervised run.
pub struct RunOpts {
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Optional label/role for display.
    pub label: Option<String>,
    /// Command to launch (default `claude`).
    pub command: String,
    /// Prompt to send once the TUI is ready (optional).
    pub initial_prompt: Option<String>,
    /// Interval between pane polls.
    pub poll: Duration,
    /// How long to wait for the TUI to come up.
    pub boot_timeout: Duration,
}

/// What the supervisor should do for a given pane state. Pure so it can
/// be tested without a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperviseAction {
    /// Real quota limit — waiting won't help; stop and hand back.
    GiveUp,
    /// Working / idle / approval / error / unknown — keep watching.
    Wait,
}

/// Map a detected pane state to the supervisor's action.
#[must_use]
pub fn action_for(state: PaneState) -> SuperviseAction {
    match state {
        PaneState::UsageLimit => SuperviseAction::GiveUp,
        _ => SuperviseAction::Wait,
    }
}

/// A short, mostly-unique id from the clock and pid — enough to name a
/// session file and tmux session without pulling a uuid dep into the CLI.
#[must_use]
pub fn short_id() -> String {
    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mixed = (ns as u64) ^ (u64::from(std::process::id()) << 21);
    format!("{:08x}", mixed & 0xffff_ffff)
}

/// Sleep up to `dur`, returning early if `stop` is set. Polls in small
/// steps so Ctrl-C stays responsive.
fn sleep_interruptible(dur: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(200);
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(step.min(deadline - Instant::now()));
    }
}

/// Removes the registry entry when the supervisor exits (normal return or
/// panic). Note: a hard kill (SIGKILL) skips this, but `th claude ls`
/// prunes dead sessions on read, so a stale file self-heals.
struct RegistryGuard(String);
impl Drop for RegistryGuard {
    fn drop(&mut self) {
        registry::remove_entry(&self.0);
        control::clear(&self.0);
    }
}

/// Launch and supervise one Claude session until `stop` is set, the
/// session exits, or a non-retryable limit is hit.
///
/// # Errors
/// On tmux launch failure or an unrecoverable tmux error mid-loop.
pub fn supervise_blocking(opts: RunOpts, stop: Arc<AtomicBool>) -> Result<()> {
    let id = short_id();
    let session = format!("claude-{id}");

    // Export the agent handle so the `smooth-agent` Claude Code plugin's
    // SessionStart hook registers this session on the th-mail bus under a
    // handle Big Smooth can address (`th msg send --to <handle>`). The
    // command is wrapped in `sh -c` by the driver, so an inline
    // assignment reaches the launched process's environment.
    let launch_cmd = format!("SMOOTH_AGENT_HANDLE={id} SMOOTH_SESSION={id} {}", opts.command);

    let mut driver = TmuxDriver::start(&session, &opts.cwd, &launch_cmd, opts.boot_timeout)?;
    driver.set_capture_max_bytes(128 * 1024);

    let entry = SessionEntry {
        id: id.clone(),
        session: session.clone(),
        socket: driver.socket().to_string(),
        cwd: opts.cwd.to_string_lossy().into_owned(),
        label: opts.label.clone(),
        pid: std::process::id(),
        started_at: Utc::now(),
    };
    registry::write_entry(&entry)?;
    let _guard = RegistryGuard(id.clone());

    println!("{} session {} ({})", "▶".green(), id.bold(), session.dimmed());
    println!("  attach with: {}", format!("th claude attach {id}").cyan());

    // Wait for the TUI to render, then send the initial prompt — but only
    // if Big Smooth is driving. In Manual/Paused the human owns input.
    if let Some(prompt) = &opts.initial_prompt {
        if control::read_mode(&id).drives() {
            let _ = driver.wait_for_idle(Duration::from_secs(1), Duration::from_millis(300), Duration::from_secs(20));
            driver.send(prompt)?;
            println!("  {} sent initial prompt", "→".green());
        }
    }

    loop {
        if stop.load(Ordering::SeqCst) {
            println!("  {} stopped", "⏹".yellow());
            break;
        }
        if !driver.is_alive() {
            println!("  {} session ended", "✓".green());
            break;
        }

        let visible = driver.capture_visible().unwrap_or_default();
        if action_for(detect_state(&visible)) == SuperviseAction::GiveUp {
            println!("  {} usage/quota limit reached — waiting won't help; leaving the session for you", "🛑".red());
            break;
        }

        sleep_interruptible(opts.poll, &stop);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_mapping() {
        assert_eq!(action_for(PaneState::UsageLimit), SuperviseAction::GiveUp);
        assert_eq!(action_for(PaneState::Working), SuperviseAction::Wait);
        assert_eq!(action_for(PaneState::Idle), SuperviseAction::Wait);
        assert_eq!(action_for(PaneState::AwaitingApproval), SuperviseAction::Wait);
        assert_eq!(action_for(PaneState::Errored), SuperviseAction::Wait);
        assert_eq!(action_for(PaneState::Unknown), SuperviseAction::Wait);
    }

    #[test]
    fn short_id_is_hex_and_unique() {
        let a = short_id();
        let b = short_id();
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "ids from successive calls should differ");
    }

    #[test]
    fn interruptible_sleep_returns_early_on_stop() {
        let stop = AtomicBool::new(true);
        let start = Instant::now();
        sleep_interruptible(Duration::from_secs(30), &stop);
        assert!(start.elapsed() < Duration::from_secs(1), "should have bailed immediately");
    }

    #[test]
    fn interruptible_sleep_waits_when_not_stopped() {
        let stop = AtomicBool::new(false);
        let start = Instant::now();
        sleep_interruptible(Duration::from_millis(300), &stop);
        assert!(start.elapsed() >= Duration::from_millis(250));
    }
}
