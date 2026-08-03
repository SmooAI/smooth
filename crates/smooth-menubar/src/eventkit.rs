//! EventKit (Calendar + Reminders) access — the TCC grant triggers (pearl
//! th-94cc4a).
//!
//! macOS only hands out an EventKit grant to a process that (a) lives in an app
//! bundle declaring the matching usage description
//! (`NSCalendarsFullAccessUsageDescription` /
//! `NSRemindersFullAccessUsageDescription`) and (b) actually *asks* —
//! `EKEventStore.requestFullAccessToEvents` / `…ToReminders`. A CLI that merely
//! reads and fails gets a silent denial with no prompt, which is exactly the
//! trap the `ical` CLI falls into when the daemon shells out to it before the
//! grant exists. So Big Smooth.app asks once at startup; the OS prompt fires,
//! the user clicks Allow, and every `ical` child process inherits the grant (TCC
//! attributes a spawned helper to its responsible process — the .app).
//!
//! Calendar and Reminders are **separate grants** — one prompt each, one row
//! each in System Settings. Granting Calendar does nothing for Reminders.
//!
//! This lives in the menu-bar crate because that's the workspace's macOS
//! quarantine: the one crate allowed `unsafe` for objc2 FFI. The reminder
//! reads and writes themselves live next door in [`crate::reminders`].

#![cfg(target_os = "macos")]

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEventStore};
use objc2_foundation::NSError;

/// How long to wait for the user to answer the OS prompt before giving up.
/// The grant still lands if they answer later — we just stop blocking on it.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// The daemon's view of an EventKit TCC grant, without leaking objc2 types to
/// callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Never asked — asking will show the OS prompt.
    NotDetermined,
    /// The user said yes (or a profile granted it).
    Granted,
    /// Denied or restricted; only System Settings can undo it.
    Denied,
}

impl Access {
    /// Whether reads will actually work.
    #[must_use]
    pub fn granted(self) -> bool {
        self == Self::Granted
    }

    /// Stable lowercase label for logs and `th doctor` output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotDetermined => "not-determined",
            Self::Granted => "granted",
            Self::Denied => "denied",
        }
    }
}

/// Authorization for one EventKit entity type. Cheap, no prompt.
fn access_for(entity: EKEntityType) -> Access {
    // SAFETY: a class method taking a plain enum; no pointers involved.
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(entity) };
    match status {
        EKAuthorizationStatus::NotDetermined => Access::NotDetermined,
        EKAuthorizationStatus::FullAccess | EKAuthorizationStatus::WriteOnly => Access::Granted,
        _ => Access::Denied,
    }
}

/// Current Calendar authorization for this process. Cheap, no prompt.
#[must_use]
pub fn calendar_access() -> Access {
    access_for(EKEntityType::Event)
}

/// Current Reminders authorization for this process. Cheap, no prompt.
///
/// Note that a `WriteOnly` grant counts as [`Access::Granted`] here even though
/// reminder *reads* would still fail — EventKit only ever returns `WriteOnly`
/// for calendar events, never for reminders, so the case can't arise.
#[must_use]
pub fn reminders_access() -> Access {
    access_for(EKEntityType::Reminder)
}

/// Ask for full access to one entity type, blocking until the user answers (or
/// [`PROMPT_TIMEOUT`]). `ask` invokes the right `requestFullAccessTo…` selector.
fn request_access(entity: EKEntityType, ask: impl FnOnce(&EKEventStore, *mut block2::DynBlock<dyn Fn(Bool, *mut NSError)>)) -> Access {
    let current = access_for(entity);
    if current != Access::NotDetermined {
        return current;
    }

    // SAFETY: plain designated initializer, no arguments to get wrong.
    let store = unsafe { EKEventStore::new() };
    let (tx, rx) = mpsc::channel();
    let completion = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        // The callback fires on an arbitrary queue; just wake the caller and
        // re-read the authoritative status below.
        let _ = tx.send(());
    });
    ask(&store, RcBlock::as_ptr(&completion));

    if rx.recv_timeout(PROMPT_TIMEOUT).is_err() {
        // Timed out: the user hasn't answered the prompt yet, so the completion
        // block may still be invoked after this function returns. Leak the one
        // block rather than bet on EventKit having copied it — a few hundred
        // bytes, once per process, against a use-after-free.
        std::mem::forget(completion);
    }
    access_for(entity)
}

/// Ask for full Calendar access, blocking until the user answers (or
/// [`PROMPT_TIMEOUT`]). Returns the resulting [`Access`].
///
/// Only shows a prompt when the caller is a bundled, signed app in a GUI login
/// session — from a bare CLI it returns [`Access::Denied`] with no UI, which is
/// why `th doctor --setup-calendar` drives Big Smooth.app rather than asking
/// itself. Already-answered states short-circuit (macOS never re-prompts).
#[must_use]
pub fn request_calendar_access() -> Access {
    request_access(EKEntityType::Event, |store, block| {
        // SAFETY: the pointer comes straight from a live `RcBlock` that outlives
        // the call, and EventKit copies the block for its async callback.
        unsafe { store.requestFullAccessToEventsWithCompletion(block) };
    })
}

/// Ask for full Reminders access. Same rules as [`request_calendar_access`] —
/// separate grant, separate prompt, driven by `th doctor --setup-reminders`.
#[must_use]
pub fn request_reminders_access() -> Access {
    request_access(EKEntityType::Reminder, |store, block| {
        // SAFETY: as above — a live `RcBlock` pointer that EventKit copies.
        unsafe { store.requestFullAccessToRemindersWithCompletion(block) };
    })
}

/// Fire an access request on a detached thread and log the outcome. `what` is
/// the label used in the log lines.
fn request_in_background(what: &'static str, current: fn() -> Access, request: fn() -> Access) {
    let before = current();
    if before != Access::NotDetermined {
        tracing::debug!(status = before.label(), "{what} access already decided");
        return;
    }
    std::thread::spawn(move || {
        let after = request();
        tracing::info!(status = after.label(), "{what} access request answered");
    });
}

/// Fire the Calendar access request on a detached thread and log the outcome —
/// what the daemon calls at startup, where blocking the boot on a modal prompt
/// would be rude and blocking the AppKit main thread would deadlock the prompt
/// itself.
pub fn request_calendar_access_in_background() {
    request_in_background("calendar", calendar_access, request_calendar_access);
}

/// The Reminders twin of [`request_calendar_access_in_background`].
pub fn request_reminders_access_in_background() {
    request_in_background("reminders", reminders_access, request_reminders_access);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_stable() {
        assert_eq!(Access::Granted.label(), "granted");
        assert_eq!(Access::Denied.label(), "denied");
        assert_eq!(Access::NotDetermined.label(), "not-determined");
    }

    #[test]
    fn only_granted_counts_as_usable() {
        assert!(Access::Granted.granted());
        assert!(!Access::Denied.granted());
        assert!(!Access::NotDetermined.granted());
    }

    #[test]
    fn status_queries_never_panic_and_never_prompt() {
        // Reading status is safe from a test binary (no bundle, no GUI); the
        // point is that both FFI calls are well-formed.
        let _ = calendar_access();
        let _ = reminders_access();
    }
}
