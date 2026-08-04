//! Auto-initiated macOS access grants (pearl th-ba764e).
//!
//! The "Set Up" menu items in [`crate`] are the deliberate path. This is the
//! reflexive one: when a tool discovers it lacks a grant, the daemon drives the
//! grant flow **itself** rather than telling the user to go run `th doctor`.
//! Same reason the menu items exist — TCC attributes a grant to the process
//! that asks, and the process that needs it is this one, not the CLI.
//!
//! [`initiate`] returns the sentence the tool should hand back in place of its
//! static setup hint, or `None` when nothing was started (already asked this
//! session, the grant was already answered, or we're headless and no prompt can
//! appear) — in which case the caller keeps its actionable text.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicU8, Ordering};

use crate::eventkit::{self, Access};

/// A macOS access grant Big Smooth can drive from in-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Calendar,
    Reminders,
    /// `chat.db` reads and external-volume workspaces. No prompt API exists —
    /// the most anyone can do is open the pane.
    FullDiskAccess,
    /// Apple Events → Messages.app, for sending.
    MessagesAutomation,
}

/// One bit per [`Grant`], set the first time it is initiated in this process:
/// an ungranted tool called ten times in a turn must pop one prompt, not ten.
/// Per-process, not persisted — a restart may legitimately ask again.
static INITIATED: AtomicU8 = AtomicU8::new(0);

/// Take the once-per-session claim for `grant`. `true` for the first caller.
fn claim(grant: Grant) -> bool {
    let bit = 1u8 << grant as u8;
    INITIATED.fetch_or(bit, Ordering::Relaxed) & bit == 0
}

/// Start the grant flow for `grant` and return what to tell the user, or `None`
/// to leave the caller's own setup hint in place.
///
/// EventKit requests run on a detached thread: they block until the user
/// answers, and the caller is an agent turn that shouldn't stall on a prompt.
#[must_use]
pub fn initiate(grant: Grant) -> Option<&'static str> {
    // Prompts and Settings deep links only land when we're the GUI app. A
    // headless daemon (launchd, CI, over SSH) can't show anything, so its tools
    // keep the "here's what to run" text.
    if !crate::enabled() {
        return None;
    }
    match grant {
        // Already-answered EventKit grants never re-prompt, so claiming to have
        // opened a prompt would be a lie — fall back to the hint instead.
        Grant::Calendar => (eventkit::calendar_access() == Access::NotDetermined && claim(grant)).then(|| {
            eventkit::request_calendar_access_in_background();
            "I just opened the macOS Calendar permission prompt — click Allow, then ask me again."
        }),
        Grant::Reminders => (eventkit::reminders_access() == Access::NotDetermined && claim(grant)).then(|| {
            eventkit::request_reminders_access_in_background();
            "I just opened the macOS Reminders permission prompt — click Allow, then ask me again."
        }),
        Grant::FullDiskAccess => claim(grant).then(|| {
            crate::open_url(crate::FDA_SETTINGS_URL);
            "I just opened System Settings → Privacy & Security → Full Disk Access — switch Big Smooth on there, then ask me again."
        }),
        Grant::MessagesAutomation => claim(grant).then(|| {
            std::thread::spawn(|| {
                let _ = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(crate::MESSAGES_PROBE).output();
            });
            "I just asked macOS for permission to control Messages — click Allow on the prompt (or switch Big Smooth on under System Settings → Privacy & Security → Automation), then ask me again."
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_grant_claims_its_own_bit_exactly_once() {
        // ponytail: the only test that touches the process-global claim state,
        // so no reset and no lock — it would fight itself if a second one grew.
        for grant in [Grant::Calendar, Grant::Reminders, Grant::FullDiskAccess, Grant::MessagesAutomation] {
            assert!(claim(grant), "{grant:?}: first claim starts the flow");
            assert!(!claim(grant), "{grant:?}: second claim is the anti-spam guard");
        }
        // All four bits distinct — a collision would silently mute one grant.
        assert_eq!(INITIATED.load(Ordering::Relaxed), 0b1111);
    }
}
