//! `get_location` — where the Mac actually is, from macOS Location Services
//! (pearl th-ecdf4d).
//!
//! [`crate::weather`] already had a location story: a keyless IP lookup. That
//! answers "where does this ISP say the traffic came from", which on a VPN, or a
//! rural ISP hubbing through a city two hours away, is a different place than the
//! user. CoreLocation answers "where is this Mac" — Wi-Fi triangulation, good to
//! roughly a block. So this tool is both a standalone answer ("where am I?") and
//! the preferred coordinate source for the weather tool.
//!
//! Named `get_*` on purpose: the engine's permission classifier treats
//! `get_`/`read_`/`list_` tools as read-only-safe, so Auto never prompts for it
//! (same reasoning as `get_current_datetime` and `get_weather`).
//!
//! ## Availability
//! macOS-only — CoreLocation exists nowhere else, so Linux/Windows never see the
//! tool. Like [`crate::reminders`], it registers even when it can't work yet: an
//! ungranted process returns actionable setup guidance rather than nothing,
//! because "switch Big Smooth on under Location Services" is something the agent
//! can relay and the user can act on. Hiding the tool would make Big Smooth claim
//! it has no idea where the user is — wrong, and unactionable.
//!
//! ## Sandbox
//! Same trusted-integration exception as `reminders`: there is no subprocess at
//! all, just in-process CoreLocation calls through [`smooth_menubar::location`]
//! (the workspace's objc2 quarantine crate), which the kernel sandbox's seatbelt
//! profile would block. It is still a normal tool call, so the permission gate
//! and the Narc hook see it like any other.

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_menubar::location::{current_location, Coord, LocationError};
use smooth_menubar::setup::{initiate, Grant};
use smooth_operator::{Tool, ToolSchema};

/// What to tell the user when Location Services isn't usable. One string so every
/// failure path funnels to the same next step. There is no `th doctor` flag for
/// this one — unlike EventKit, the Location toggle is a plain switch in System
/// Settings, and the in-app prompt ([`initiate`]) is the fast path.
const SETUP_HINT: &str = "Turn it on in System Settings › Privacy & Security › Location Services (switch on Big Smooth), then ask me again.";

/// `get_location` — the device's current position.
pub struct GetLocationTool;

#[async_trait]
impl Tool for GetLocationTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_location".into(),
            description: "Get the user's CURRENT physical location from macOS Location Services (precise — Wi-Fi/GPS, not an IP guess). Use it whenever you need to know where they are: \"where am I\", anything about what's nearby, or to get coordinates for a local search. Takes no arguments. Returns latitude and longitude."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Read-only, and each call builds its own CLLocationManager.
        true
    }

    async fn execute(&self, _arguments: Value) -> anyhow::Result<String> {
        // A failed lookup is an answer, not a tool crash: the model can act on
        // "grant Location access", but not on a hard error.
        Ok(match coordinates().await {
            Ok(c) => render(c),
            Err(e) => explain(e),
        })
    }
}

/// The device's coordinates, off the async runtime.
///
/// CoreLocation is run-loop driven and [`current_location`] blocks up to its fix
/// timeout, so it must not run on a worker thread. Shared with
/// [`crate::weather`], which prefers this over its IP fallback.
///
/// # Errors
/// Whatever [`current_location`] reports — plus [`LocationError::Timeout`] if the
/// blocking task itself failed to complete (ponytail: a join failure and a fix
/// that never arrived are the same thing to every caller, and this way the tool
/// can't panic the turn).
pub async fn coordinates() -> Result<Coord, LocationError> {
    tokio::task::spawn_blocking(current_location).await.unwrap_or(Err(LocationError::Timeout))
}

/// A found position, as the model sees it. Six decimals is ~10cm — well past
/// what Wi-Fi triangulation resolves, but it keeps the value round-trippable
/// into the weather tool without the model rewriting it.
fn render(c: Coord) -> String {
    format!(
        "{{\"latitude\":{:.6},\"longitude\":{:.6},\"source\":\"macOS Location Services\"}}",
        c.lat, c.lon
    )
}

/// Turn a failure into the sentence the agent should relay, asking for the grant
/// in-process where that's possible.
///
/// The prompt has to come from *this* process for the grant to land on it (pearl
/// th-ba764e), so a not-yet-asked state opens it here rather than sending the
/// user off to go find a setting.
fn explain(e: LocationError) -> String {
    match e {
        // Nobody has asked yet — ask, once per session.
        LocationError::NotDetermined => initiate(Grant::Location).map_or_else(|| format!("{e}. {SETUP_HINT}"), ToOwned::to_owned),
        // Denied never re-prompts, and a timeout is usually Location Services
        // switched off machine-wide; both end at the same System Settings pane.
        LocationError::Denied | LocationError::Timeout => format!("{e}. {SETUP_HINT}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn schema_takes_no_arguments_and_reads_as_safe() {
        let s = GetLocationTool.schema();
        assert_eq!(s.name, "get_location");
        assert!(s.name.starts_with("get_"), "the `get_` prefix is what keeps Auto from prompting");
        assert_eq!(s.parameters["required"].as_array().unwrap().len(), 0);
        assert_eq!(s.parameters["properties"].as_object().unwrap().len(), 0);
        assert!(GetLocationTool.is_concurrent_safe());
    }

    #[test]
    fn render_emits_parseable_coordinates() {
        let out = render(Coord { lat: 39.9553, lon: -86.0134 });
        assert_eq!(out, r#"{"latitude":39.955300,"longitude":-86.013400,"source":"macOS Location Services"}"#);
        // Hand-built JSON, so prove it actually parses rather than trusting the
        // format string.
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["source"], "macOS Location Services");
    }

    #[test]
    fn render_keeps_the_southern_and_eastern_hemispheres_signed() {
        let out = render(Coord { lat: -33.8688, lon: 151.2093 });
        assert!(out.contains(r#""latitude":-33.868800"#), "{out}");
        assert!(out.contains(r#""longitude":151.209300"#), "{out}");
    }

    #[test]
    fn every_failure_names_the_next_step() {
        // The failure mode this guards: "I couldn't get your location" with
        // nothing the user can do about it.
        for e in [LocationError::Denied, LocationError::Timeout] {
            let out = explain(e);
            assert!(out.contains("Location Services"), "{e:?}: {out}");
            assert!(out.contains("System Settings"), "{e:?}: {out}");
        }
        // NotDetermined either opens the prompt or falls back to the same hint.
        let out = explain(LocationError::NotDetermined);
        assert!(out.contains("System Settings") || out.contains("permission prompt"), "{out}");
    }

    #[tokio::test]
    async fn execute_without_a_grant_answers_instead_of_erroring() {
        // A test binary is never TCC-granted, so this is the ungranted path.
        let out = GetLocationTool.execute(json!({})).await.unwrap();
        if out.contains("latitude") {
            return; // granted on this machine — the ungranted path isn't reachable
        }
        assert!(out.contains("Location"), "{out}");
    }
}
