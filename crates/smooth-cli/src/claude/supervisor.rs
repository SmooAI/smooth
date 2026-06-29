//! The 1:1 supervisor loop: launch a Claude Code TUI in an isolated tmux
//! session and keep it alive. On the account-wide rate-limit throttle,
//! back off with jitter (via the shared [`RateLimitGovernor`]) and resend
//! the last message until it lands.
//!
//! This is the degenerate case of every topology — one supervisor, one
//! session, one governor. The 1:N farm reuses the same pieces with one
//! `Arc<RateLimitGovernor>` shared across N supervisors.
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

use anyhow::Result;
use chrono::Utc;
use owo_colors::OwoColorize;
use smooth_tmux::TmuxDriver;

use super::detect::{detect_state, extract_last_user_message, PaneState};
use super::governor::RateLimitGovernor;
use super::registry::{self, SessionEntry};

/// How long to wait after a resend before re-evaluating the pane, so the
/// supervisor doesn't re-detect the stale throttle line (still on screen)
/// and resend on top of itself before the model has reacted.
const RESEND_SETTLE: Duration = Duration::from_secs(8);

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
    /// Transient throttle — back off and resend the last message.
    Rescue,
    /// Real quota limit — backing off won't help; stop and hand back.
    GiveUp,
    /// Working / idle / approval / unknown — keep watching.
    Wait,
}

/// Map a detected pane state to the supervisor's action.
#[must_use]
pub fn action_for(state: PaneState) -> SuperviseAction {
    match state {
        PaneState::RateLimited => SuperviseAction::Rescue,
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
/// steps so Ctrl-C is responsive even during a long backoff.
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

    let mut driver = TmuxDriver::start(&session, &opts.cwd, &opts.command, opts.boot_timeout)?;
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

    // Wait for the TUI to render, then send the initial prompt.
    let mut last_message = opts.initial_prompt.clone();
    if let Some(prompt) = &opts.initial_prompt {
        let _ = driver.wait_for_idle(Duration::from_secs(1), Duration::from_millis(300), Duration::from_secs(20));
        driver.send(prompt)?;
        println!("  {} sent initial prompt", "→".green());
    }

    let governor = RateLimitGovernor::new();

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
        match action_for(detect_state(&visible)) {
            SuperviseAction::Rescue => {
                // Prefer the message we sent; fall back to scraping the
                // last user turn out of full scrollback.
                let msg = last_message
                    .clone()
                    .or_else(|| extract_last_user_message(&driver.capture().unwrap_or_default()));
                let wait = governor.record_rate_limit();
                println!(
                    "  {} rate limited (#{}) — backing off {}",
                    "⏳".yellow(),
                    governor.consecutive(),
                    fmt_dur(wait).yellow()
                );
                sleep_interruptible(wait, &stop);
                if stop.load(Ordering::SeqCst) {
                    continue;
                }
                match &msg {
                    Some(m) => {
                        driver.send(m)?;
                        last_message = Some(m.clone());
                        println!("  {} resent last message", "↻".green());
                    }
                    None => {
                        println!("  {} couldn't determine the last message — attach and resend manually", "⚠".red());
                    }
                }
                // Let the model react before re-evaluating.
                sleep_interruptible(RESEND_SETTLE, &stop);
            }
            SuperviseAction::GiveUp => {
                println!(
                    "  {} usage/quota limit reached — backing off won't help; leaving the session for you",
                    "🛑".red()
                );
                break;
            }
            SuperviseAction::Wait => {
                if governor.consecutive() > 0 {
                    governor.record_success();
                    println!("  {} recovered — backoff reset", "✓".green());
                }
            }
        }

        sleep_interruptible(opts.poll, &stop);
    }

    Ok(())
}

fn fmt_dur(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_mapping() {
        assert_eq!(action_for(PaneState::RateLimited), SuperviseAction::Rescue);
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

    #[test]
    fn fmt_dur_formats_minutes() {
        assert_eq!(fmt_dur(Duration::from_secs(45)), "45s");
        assert_eq!(fmt_dur(Duration::from_secs(125)), "2m5s");
    }
}
