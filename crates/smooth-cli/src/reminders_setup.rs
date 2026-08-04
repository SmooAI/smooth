//! `th doctor --setup-reminders` — make Big Smooth's macOS Reminders tool work
//! (pearl th-94cc4a, reminders slice).
//!
//! Only one thing has to be true before the daemon's `reminders` tool works:
//! macOS has granted **Big Smooth.app** Reminders access. Unlike the calendar
//! setup next door there is no CLI to install — reminders are read and written
//! through EventKit in-process (`smooth_menubar::reminders`).
//!
//! `th` cannot own the grant itself: a TCC grant is attributed to the app bundle
//! that asks, and a bare CLI asking gets a silent denial. So this reports the
//! current status, drives **the app** into asking (the daemon calls
//! `EKEventStore.requestFullAccessToReminders` at startup — see
//! `smooth_menubar::eventkit`), and says what the human still has to click.
//!
//! Reminders is a **separate** grant from Calendar: `--setup-calendar` does
//! nothing for it, and vice versa.

#![cfg(target_os = "macos")]

use std::process::Command;

use anstream::println;
use anyhow::Result;
use owo_colors::OwoColorize;
use smooth_menubar::eventkit::{reminders_access, Access};

/// Run the whole setup: report the grant, then drive the app into asking.
///
/// # Errors
/// Never fails outright — a missing app bundle is reported with the next step,
/// so a half-configured Mac still gets a full readiness picture.
pub fn run() -> Result<()> {
    // 1. Where the grant stands right now. `th` is not the bundle, so this is
    //    `th`'s own status — but TCC keys Reminders on the *responsible* app, so
    //    on a Mac where the app has been allowed this usually reads granted too.
    let status = reminders_access();
    match status {
        Access::Granted => println!("  {} Reminders access: {}", "✓".green().bold(), status.label()),
        Access::Denied => {
            println!("  {} Reminders access: {} — macOS was told no", "✗".red().bold(), status.label());
            println!(
                "    {} re-enable it in System Settings → Privacy & Security → {}",
                "→".cyan(),
                "Reminders".bold()
            );
        }
        Access::NotDetermined => println!("  {} Reminders access: {} — nobody has asked yet", "→".cyan(), status.label()),
    }

    // 2. The app bundle — the only identity macOS will grant Reminders access to.
    let Some(app) = crate::calendar_setup::app_bundle() else {
        println!("  {} Big Smooth.app: not installed", "✗".red().bold());
        println!(
            "    {} install it first, then re-run this: {}",
            "→".cyan(),
            "scripts/macos/install-local.sh".bold()
        );
        println!(
            "\n  {} the Reminders permission can only be granted to the app bundle — a bare CLI",
            "⚠".yellow().bold()
        );
        println!("     asking gets a silent denial, so the tool stays unusable until the app exists.");
        return Ok(());
    };
    println!("  {} Big Smooth.app: {}", "✓".green().bold(), app.display().to_string().dimmed());

    // 3. Drive the grant. The daemon asks EventKit at startup, so the trigger is
    //    simply "(re)launch the app in this GUI session".
    if crate::calendar_setup::app_is_running() {
        println!("  {} Big Smooth is already running — it asks for Reminders access at startup,", "→".cyan());
        println!("    so restart it to trigger the prompt:");
        println!(
            "      {}",
            format!("osascript -e 'quit app \"Big Smooth\"' && open -a \"{}\"", app.display()).bold()
        );
    } else {
        println!("  {} launching Big Smooth so it asks macOS for Reminders access…", "→".cyan());
        if let Err(e) = Command::new("/usr/bin/open").arg(&app).status() {
            println!("    {} couldn't launch it ({e}) — open it from Finder instead.", "✗".red().bold());
        }
    }

    println!("\n  {}", "What you have to click:".bold());
    println!(
        "    1. macOS shows “Big Smooth would like to access your reminders” — choose {}.",
        "Allow".green().bold()
    );
    println!("    2. Verify later in System Settings → Privacy & Security → Reminders.");
    println!(
        "\n  {} the prompt only appears in a GUI login session on the Mac itself (never over SSH),",
        "⚠".yellow().bold()
    );
    println!("     and macOS asks exactly once — if it was denied, re-enable it in System Settings.");
    println!(
        "\n  {} this is a separate grant from Calendar — {} does not cover it.",
        "⚠".yellow().bold(),
        "th doctor --setup-calendar".bold()
    );
    println!("\n  Then ask Big Smooth “what's on my reminders?”.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_probe_never_panics_and_never_prompts() {
        // Machine-dependent result; the invariant is that reading it is total
        // and silent (a prompt from `th` would be a silent denial anyway).
        let _ = reminders_access();
    }

    #[test]
    fn setup_reuses_the_calendar_bundle_lookup() {
        // Both grants attach to the same app bundle — there must not be a second
        // copy of "where is Big Smooth.app" that can drift from the first.
        let _ = crate::calendar_setup::app_bundle();
        let _ = crate::calendar_setup::app_is_running();
    }
}
