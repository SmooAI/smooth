//! `th doctor --setup-imessage` — make Big Smooth's macOS Messages tool work
//! (pearl th-1665ed).
//!
//! Two separate TCC grants have to be in place before the daemon's `imessage`
//! tool works, and **neither can be granted programmatically** — the TCC database
//! is SIP-protected. So, exactly like [`crate::fda`], the most this can do is
//! DETECT what's missing and DRIVE the one-time prompts:
//!
//! 1. **Full Disk Access** — `~/Library/Messages/chat.db` is FDA-gated. Without
//!    it every read is a silent `EPERM`. Not promptable at all: the human must
//!    add the binary in System Settings. This opens the pane and reveals the
//!    binaries to drag in.
//! 2. **Automation (Apple Events → Messages.app)** — sending is AppleScript, and
//!    macOS asks once, on the first Apple Event. That one IS promptable, so this
//!    fires a harmless probe (`get name`, which sends nothing) to make the prompt
//!    appear now rather than in the middle of an agent turn.
//!
//! Like the calendar setup next door, the grants are attributed to **Big
//! Smooth.app** — a bare CLI asking gets a silent denial — so this reports the
//! bundle's presence too.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use owo_colors::OwoColorize;

/// The app bundle that owns the TCC grants.
const APP_NAME: &str = "Big Smooth.app";

/// A probe that sends nothing but still counts as an Apple Event to Messages —
/// which is what makes macOS show the Automation prompt.
const PROBE_SCRIPT: &str = r#"tell application "Messages" to get name"#;

/// The installed app bundle, if present (`~/Applications` first, then
/// `/Applications`).
///
/// ponytail: a near-twin of `calendar_setup::app_bundle` (pearl th-94cc4a, in
/// flight on its own branch). Left duplicated rather than blocking on that PR;
/// fold both into one helper when the second one lands.
fn app_bundle() -> Option<PathBuf> {
    dirs_next::home_dir()
        .map(|h| h.join("Applications").join(APP_NAME))
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/Applications").join(APP_NAME)))
        .find(|p| p.is_dir())
}

/// Fire the Apple Event that makes macOS ask for the Automation grant.
///
/// Returns the `osascript` stderr on failure. A denial here is the expected
/// first-run outcome, not an error — the point is that the prompt appeared.
fn prime_automation_grant() -> Result<(), String> {
    let out = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(PROBE_SCRIPT)
        .output()
        .map_err(|e| format!("could not run osascript: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
}

/// Run the whole setup: report Full Disk Access, then drive the Automation grant.
///
/// # Errors
/// Never fails outright — every missing piece is reported with the next step, so
/// a half-configured Mac still gets a full readiness picture.
pub fn run() -> Result<()> {
    // 1. The database + Full Disk Access.
    let path = smooth_tools::imessage::chat_db_path();
    match path.as_deref().map(|p| (p, smooth_tools::imessage::probe(p))) {
        Some((p, Ok(()))) => {
            println!("  {} chat.db: readable ({})", "✓".green().bold(), p.display().to_string().dimmed());
        }
        Some((p, Err(smooth_tools::imessage::Unavailable::Missing))) => {
            println!("  {} chat.db: not found at {}", "✗".red().bold(), p.display());
            println!("    {} open Messages.app and sign in once, then re-run this.", "→".cyan());
        }
        Some((p, Err(smooth_tools::imessage::Unavailable::Denied))) => {
            println!("  {} chat.db: {} — Full Disk Access is not granted", "✗".red().bold(), p.display());
            report_fda_fix();
        }
        None => println!("  {} chat.db: cannot determine the home directory", "✗".red().bold()),
    }

    // 2. The app bundle — the identity macOS attributes both grants to.
    if let Some(app) = app_bundle() {
        println!("  {} {APP_NAME}: {}", "✓".green().bold(), app.display().to_string().dimmed());
    } else {
        println!("  {} {APP_NAME}: not installed", "✗".red().bold());
        println!(
            "    {} install it first, then re-run this: {}",
            "→".cyan(),
            "scripts/macos/install-local.sh".bold()
        );
        println!(
            "\n  {} the grants below can only attach to the app bundle — a bare CLI asking",
            "⚠".yellow().bold()
        );
        println!("     gets a silent denial, so the tool stays unusable until the app exists.");
    }

    // 3. Automation (Apple Events → Messages.app), for sending.
    println!("  {} priming the Messages automation prompt…", "→".cyan());
    match prime_automation_grant() {
        Ok(()) => println!("  {} Messages automation: allowed (sending will work)", "✓".green().bold()),
        Err(e) => {
            println!("  {} Messages automation: not granted yet", "✗".red().bold());
            println!("    {}", e.dimmed());
            println!("    {} allow it in System Settings → Privacy & Security → {}", "→".cyan(), "Automation".bold());
        }
    }

    println!("\n  {}", "What you have to click:".bold());
    println!(
        "    1. Full Disk Access: add the binaries above with {}, and toggle them {}.",
        "+".bold(),
        "on".green().bold()
    );
    println!("    2. “Big Smooth wants to control Messages” → choose {}.", "Allow".green().bold());
    println!(
        "\n  {} both prompts only appear in a GUI login session on the Mac itself (never over",
        "⚠".yellow().bold()
    );
    println!("     SSH), and macOS asks exactly once — if denied, re-enable in System Settings.");
    println!(
        "\n  {} reading is ON by default once Full Disk Access is granted: Big Smooth can read",
        "🔒".bold()
    );
    println!("     every message in your history. Revoke by removing it from Full Disk Access.");
    println!("\n  Then ask Big Smooth “what are my recent texts?”.");
    Ok(())
}

/// The Full Disk Access half of the fix — reuses the `--fix-fda` machinery so
/// there is one implementation of "open the pane and reveal the binaries".
fn report_fda_fix() {
    for target in crate::fda::grant_targets() {
        println!("      {} {}", "•".dimmed(), target.display());
    }
    println!(
        "    {} opening System Settings → Privacy & Security → {}…",
        "→".cyan(),
        "Full Disk Access".bold()
    );
    if let Err(e) = crate::fda::open_fda_settings() {
        println!("    {} couldn't open it ({e}) — open System Settings by hand.", "✗".red().bold());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn app_lookup_does_not_panic() {
        // Machine-dependent result; the invariant is that it's total.
        let _ = app_bundle();
    }

    #[test]
    fn the_probe_script_sends_nothing() {
        // Load-bearing: setup must never text a human. If someone "improves"
        // this into a real send, this fails.
        assert!(!PROBE_SCRIPT.contains("send"), "{PROBE_SCRIPT}");
        assert!(!PROBE_SCRIPT.contains("participant"), "{PROBE_SCRIPT}");
        assert!(PROBE_SCRIPT.contains("get name"));
    }

    #[test]
    fn setup_reuses_the_fda_grant_targets() {
        // The FDA half must not grow its own copy of "which binaries need the
        // grant" — that list lives in `fda` and is shared with `--fix-fda`.
        assert!(!crate::fda::grant_targets().is_empty());
    }

    #[test]
    fn the_tool_and_the_setup_command_agree_on_the_database_path() {
        // A drift between these two is the classic "setup says ready, tool says
        // not set up" bug.
        std::env::set_var("SMOOTH_CHAT_DB", "/tmp/th-setup-test-chat.db");
        assert_eq!(smooth_tools::imessage::chat_db_path(), Some(PathBuf::from("/tmp/th-setup-test-chat.db")));
        std::env::remove_var("SMOOTH_CHAT_DB");
    }
}
