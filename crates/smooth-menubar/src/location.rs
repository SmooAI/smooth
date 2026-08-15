//! CoreLocation — the device's real position from macOS Location Services
//! (pearl th-ecdf4d).
//!
//! The IP-geo fallback the weather tool ships with resolves to whatever the
//! ISP's egress claims, which on a VPN (or a rural ISP hubbing through a city
//! two hours away) is simply wrong. Location Services knows where the Mac
//! actually is — Wi-Fi triangulation on a laptop, good to a block or so.
//!
//! Same shape as [`crate::eventkit`], and for the same reason: this lives in the
//! menu-bar crate because that is the workspace's macOS quarantine — the one
//! crate allowed `unsafe` for objc2 FFI. Callers get [`Coord`], [`Access`] and
//! [`LocationError`]; no objc2 type escapes.
//!
//! Two macOS facts drive the design:
//!
//! 1. **A grant is attributed to the process that asks**, and only a bundled app
//!    declaring `NSLocationWhenInUseUsageDescription` can raise the prompt. A
//!    bare CLI asking gets a silent denial, which is why [`initiate_location`]
//!    is meant to run inside Big Smooth.app.
//! 2. **CoreLocation is run-loop driven.** There is no blocking "get me a fix"
//!    call: you start updates and the framework fills `manager.location` from
//!    the run loop of the thread the manager was created on. So every entry
//!    point here creates its manager and pumps *that* thread's run loop
//!    ([`pump_until`]) — which also means every one of them **blocks**, and
//!    callers on an async runtime must go through `spawn_blocking`.

#![cfg(target_os = "macos")]

use std::fmt;
use std::time::{Duration, Instant};

use objc2_core_location::{CLAuthorizationStatus, CLLocationManager};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};

use crate::eventkit::Access;

/// How long to wait for a position fix before giving up. Wi-Fi triangulation is
/// usually sub-second and a warm manager answers immediately; 5s is the budget
/// for a cold radio, and it caps how long an agent turn can stall.
const FIX_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to keep a manager alive waiting for the user to answer the OS
/// prompt. Same 2 minutes [`crate::eventkit`] gives its prompts.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// One slice of run-loop pumping between polls. Short enough that a fix is
/// noticed promptly, long enough that the loop isn't a spin.
const PUMP_SLICE: Duration = Duration::from_millis(100);

/// A position, in degrees. The whole point of this module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    pub lat: f64,
    pub lon: f64,
}

/// Why a location lookup produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationError {
    /// Denied or restricted. Only System Settings can undo it — macOS will not
    /// re-prompt.
    Denied,
    /// Nobody has asked yet. [`initiate_location`] is what asks.
    NotDetermined,
    /// Granted, but no fix arrived inside [`FIX_TIMEOUT`] — Location Services
    /// switched off machine-wide, or no Wi-Fi to triangulate against.
    Timeout,
}

impl LocationError {
    /// Stable lowercase label for logs.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::NotDetermined => "not-determined",
            Self::Timeout => "timeout",
        }
    }
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::Denied => "Location access was denied to Big Smooth",
            Self::NotDetermined => "Location access hasn't been granted to Big Smooth yet",
            Self::Timeout => "Location Services didn't return a fix (it may be switched off for this Mac)",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for LocationError {}

/// This process's Location Services authorization. Cheap, and **never prompts**.
///
/// Instantiating a manager to read an instance property is deliberate: the class
/// method spelling is deprecated, and constructing a `CLLocationManager` costs
/// nothing until updates are started.
#[must_use]
pub fn location_access() -> Access {
    // SAFETY: a plain designated initializer with no arguments to get wrong.
    let manager = unsafe { CLLocationManager::new() };
    access_of(&manager)
}

/// Map one manager's `authorizationStatus` onto the shared [`Access`] enum.
fn access_of(manager: &CLLocationManager) -> Access {
    // SAFETY: an instance getter returning a plain C enum; no pointers involved.
    let status = unsafe { manager.authorizationStatus() };
    match status {
        CLAuthorizationStatus::NotDetermined => Access::NotDetermined,
        // `AuthorizedAlways` and `AuthorizedWhenInUse` are the two yes answers;
        // `Restricted` (MDM/parental controls) and `Denied` are both no.
        CLAuthorizationStatus::AuthorizedAlways | CLAuthorizationStatus::AuthorizedWhenInUse => Access::Granted,
        _ => Access::Denied,
    }
}

/// The device's current position. **Blocks** for up to [`FIX_TIMEOUT`] — call it
/// from `spawn_blocking`, never on an async runtime's worker thread.
///
/// # Errors
/// [`LocationError::Denied`] / [`LocationError::NotDetermined`] when the TCC
/// grant isn't there (checked *before* touching the radio, so an unbundled or
/// headless process exits here rather than pumping a run loop for nothing), and
/// [`LocationError::Timeout`] when the grant exists but no fix arrives.
pub fn current_location() -> Result<Coord, LocationError> {
    // SAFETY: plain designated initializer, no arguments.
    let manager = unsafe { CLLocationManager::new() };
    match access_of(&manager) {
        Access::Granted => {}
        Access::NotDetermined => return Err(LocationError::NotDetermined),
        Access::Denied => return Err(LocationError::Denied),
    }

    // `startUpdatingLocation` rather than `requestLocation`: the latter delivers
    // its one-shot answer through a delegate we'd otherwise have to define, while
    // continuous updates populate `manager.location` for anyone to poll. Stopped
    // again below, so the radio stays on only for the length of this call.
    //
    // SAFETY: both are argument-free instance methods on a live manager.
    unsafe { manager.startUpdatingLocation() };
    let fix = pump_until(FIX_TIMEOUT, || {
        // SAFETY: `location` returns an autoreleased CLLocation or nil, which
        // objc2 models as the `Option<Retained<_>>` we're matching on;
        // `coordinate` is a plain struct getter on that live object.
        let loc = unsafe { manager.location() }?;
        let c = unsafe { loc.coordinate() };
        plausible(c.latitude, c.longitude).then_some(Coord {
            lat: c.latitude,
            lon: c.longitude,
        })
    });
    // SAFETY: as above.
    unsafe { manager.stopUpdatingLocation() };

    fix.ok_or(LocationError::Timeout)
}

/// Whether a coordinate is a real fix rather than CoreLocation's empty one.
///
/// ponytail: (0, 0) is a genuine point in the Gulf of Guinea, and we call it
/// invalid anyway — from this API it means "nothing yet", and reporting the
/// user's weather from open ocean is the worse failure.
fn plausible(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && (lat != 0.0 || lon != 0.0)
}

/// Ask macOS for Location Services access, blocking until the user answers (or
/// [`PROMPT_TIMEOUT`]). Already-answered states short-circuit — macOS never
/// re-prompts, so a `denied` can only be undone in System Settings.
///
/// Only shows a prompt when the caller is a bundled, signed app declaring
/// `NSLocationWhenInUseUsageDescription` in a GUI login session; from a bare CLI
/// it returns without UI.
#[must_use]
pub fn request_location_access() -> Access {
    // SAFETY: plain designated initializer, no arguments.
    let manager = unsafe { CLLocationManager::new() };
    let current = access_of(&manager);
    if current != Access::NotDetermined {
        return current;
    }
    // SAFETY: argument-free instance method; it returns immediately and the
    // answer lands on this thread's run loop, which is why the manager has to
    // stay alive (and the loop has to be pumped) below.
    unsafe { manager.requestWhenInUseAuthorization() };
    pump_until(PROMPT_TIMEOUT, || {
        let now = access_of(&manager);
        (now != Access::NotDetermined).then_some(now)
    })
    .unwrap_or(Access::NotDetermined)
}

/// Fire the Location Services prompt on a detached thread and log the outcome —
/// the CoreLocation twin of [`crate::eventkit::request_calendar_access_in_background`].
///
/// Detached because [`request_location_access`] blocks on a human, and both
/// callers (the daemon's tool path via [`crate::setup`], and a future menu item)
/// are places where stalling is not an option. Blocking the AppKit main thread
/// would additionally deadlock the very prompt it waits for.
pub fn initiate_location() {
    let before = location_access();
    if before != Access::NotDetermined {
        tracing::debug!(status = before.label(), "location access already decided");
        return;
    }
    std::thread::spawn(move || {
        let after = request_location_access();
        tracing::info!(status = after.label(), "location access request answered");
    });
}

/// Run this thread's run loop in slices until `ready` yields a value or `timeout`
/// expires. The one place CoreLocation's async delivery is turned into a
/// blocking call.
fn pump_until<T>(timeout: Duration, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = ready() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        // SAFETY: a framework-owned immortal constant string.
        let mode = unsafe { NSDefaultRunLoopMode };
        let until = NSDate::dateWithTimeIntervalSinceNow(PUMP_SLICE.as_secs_f64());
        // `false` means the loop had no input sources to run at all (nothing has
        // attached to this thread's run loop yet) and returned instantly — sleep
        // the slice ourselves so the loop can't become a busy spin.
        if !NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &until) {
            std::thread::sleep(PUMP_SLICE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_labels_and_messages_are_stable_and_actionable() {
        assert_eq!(LocationError::Denied.label(), "denied");
        assert_eq!(LocationError::NotDetermined.label(), "not-determined");
        assert_eq!(LocationError::Timeout.label(), "timeout");
        // Each message has to say what happened, not just that something did.
        for e in [LocationError::Denied, LocationError::NotDetermined, LocationError::Timeout] {
            assert!(e.to_string().len() > 20, "{e:?}: {e}");
        }
    }

    #[test]
    fn plausible_rejects_the_empty_fix_and_out_of_range_values() {
        assert!(plausible(39.955, -86.013), "a real fix");
        assert!(plausible(-33.87, 151.21), "southern/eastern hemispheres are fine");
        assert!(!plausible(0.0, 0.0), "CoreLocation's empty coordinate must not be reported");
        assert!(!plausible(91.0, 0.5), "latitude out of range");
        assert!(!plausible(45.0, 181.0), "longitude out of range");
        assert!(!plausible(f64::NAN, f64::NAN), "NaN is never a position");
    }

    #[test]
    fn status_queries_never_panic_and_never_prompt() {
        // Reading status is safe from a test binary (no bundle, no GUI); the
        // point is that the FFI call is well-formed and total.
        let _ = location_access();
    }

    #[test]
    fn an_ungranted_process_fails_fast_instead_of_pumping_for_five_seconds() {
        // A test binary is not the app bundle, so this is the ungranted path.
        // Load-bearing: the grant check has to come BEFORE the run-loop pump, or
        // every denied call costs FIX_TIMEOUT.
        if location_access().granted() {
            return; // granted on this machine — the ungranted path isn't reachable
        }
        let start = Instant::now();
        let err = current_location().expect_err("an ungranted process cannot have a fix");
        assert!(matches!(err, LocationError::Denied | LocationError::NotDetermined), "{err:?}");
        assert!(start.elapsed() < FIX_TIMEOUT, "returned in {:?}, should be immediate", start.elapsed());
    }

    #[test]
    fn pump_until_returns_early_and_honours_its_deadline() {
        // Ready immediately: no run-loop slice at all.
        assert_eq!(pump_until(Duration::from_secs(30), || Some(7)), Some(7));
        // Never ready: bounded by the timeout rather than hanging.
        let start = Instant::now();
        assert_eq!(pump_until(Duration::from_millis(250), || None::<u8>), None);
        assert!(start.elapsed() >= Duration::from_millis(200), "returned too early");
        assert!(start.elapsed() < Duration::from_secs(5), "overshot the deadline");
    }
}
