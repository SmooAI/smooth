//! Who owns the Big Smooth daemon on this machine, and what that daemon can do
//! (ADR-012).
//!
//! **Ownership.** Big Smooth.app bundles its own daemon and releases on its own
//! cadence. When the app is installed it owns the machine's one daemon, so `th`
//! uses it (launching the app if it isn't running) instead of starting a
//! competitor that would win the single-instance lock and lose the app's macOS
//! TCC grants. Without the app (Linux, servers, CLI-only installs), or with
//! `daemon.prefer_own` set, `th` runs its own daemon. The decision is
//! [`decide`] over an injectable [`AppProbe`], so it is testable without an app
//! on the machine.
//!
//! **Capabilities.** The daemon reports `GET /api/capabilities` as
//! [`DaemonCapabilities`]. Clients feature-detect against the names in
//! [`CAPABILITIES`] instead of assuming lockstep releases. A daemon that predates
//! the endpoint reports nothing, and every capability-gated feature degrades
//! with [`missing_capability_message`].

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The desktop app bundle's directory name.
pub const APP_NAME: &str = "Big Smooth.app";

/// `/cd` and `/pwd`: a session's working directory (`/api/session/cwd`).
pub const CAP_SESSION_CWD: &str = "session.cwd";
/// The Plan/Auto toggle (`/api/session/mode`).
pub const CAP_SESSION_MODE: &str = "session.mode";
/// Per-session workspace roots plus the caller's PATH
/// (`POST /api/session/workspaces`).
pub const CAP_SESSION_WORKSPACES: &str = "session.workspaces";

/// One capability a daemon can advertise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// The wire name. Never reused for different behavior.
    pub name: &'static str,
    /// The first daemon version that advertises it, quoted in upgrade hints.
    pub since: &'static str,
    /// What the user loses without it, phrased to start a sentence.
    pub feature: &'static str,
}

/// Every capability this build's daemon serves. The daemon advertises all of
/// them, and clients look up `since`/`feature` here for their messages.
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: CAP_SESSION_CWD,
        since: "0.73.0",
        feature: "Changing a session's directory (/cd)",
    },
    Capability {
        name: CAP_SESSION_MODE,
        since: "0.73.0",
        feature: "Switching Plan/Auto mode",
    },
    Capability {
        name: CAP_SESSION_WORKSPACES,
        since: "0.73.0",
        feature: "Adding repositories to a session (/workspace add) and using your shell PATH",
    },
];

/// Look up a capability by wire name.
#[must_use]
pub fn capability(name: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|c| c.name == name)
}

/// The `GET /api/capabilities` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonCapabilities {
    /// The daemon's version. Empty when the daemon predates the endpoint.
    #[serde(default)]
    pub version: String,
    /// Capability names this daemon serves.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl DaemonCapabilities {
    /// What a daemon of `version` built from this source advertises.
    #[must_use]
    pub fn current(version: &str) -> Self {
        Self {
            version: version.to_string(),
            capabilities: CAPABILITIES.iter().map(|c| c.name.to_string()).collect(),
        }
    }

    /// Whether the daemon advertised `name`.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c == name)
    }

    /// The version for messages; a pre-ADR-012 daemon has none.
    #[must_use]
    pub fn version_label(&self) -> String {
        if self.version.trim().is_empty() {
            "an older build that predates capability reporting".to_string()
        } else {
            self.version.clone()
        }
    }
}

/// Who provides the machine's daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOwner {
    /// `th` runs its own (`th up` / `th code` autostart).
    Cli,
    /// Big Smooth.app owns it. `running` is whether the app is up right now.
    App { bundle: PathBuf, running: bool },
}

/// What [`decide`] needs to know about the desktop app.
pub trait AppProbe {
    /// The installed app bundle, if any.
    fn app_bundle(&self) -> Option<PathBuf>;
    /// Whether a process from the app bundle is running.
    fn app_running(&self) -> bool;
}

/// Decide who owns the daemon. Only consulted when nothing is serving yet: a
/// live daemon at the advertised address is used whoever started it.
#[must_use]
pub fn decide(probe: &dyn AppProbe, prefer_own: bool) -> DaemonOwner {
    if prefer_own {
        return DaemonOwner::Cli;
    }
    match probe.app_bundle() {
        None => DaemonOwner::Cli,
        Some(bundle) => DaemonOwner::App {
            bundle,
            running: probe.app_running(),
        },
    }
}

/// The real machine: `~/Applications` then `/Applications` on macOS, and no
/// app anywhere else.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl AppProbe for SystemProbe {
    fn app_bundle(&self) -> Option<PathBuf> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        dirs_next::home_dir()
            .map(|h| h.join("Applications").join(APP_NAME))
            .into_iter()
            .chain(std::iter::once(PathBuf::from("/Applications").join(APP_NAME)))
            .find(|p| p.is_dir())
    }

    fn app_running(&self) -> bool {
        // Any process from inside the bundle: the native app's daemon lives in
        // Contents/MacOS, the Electron app's in Contents/Resources.
        Command::new("/usr/bin/pgrep")
            .arg("-f")
            .arg(format!("{APP_NAME}/Contents/"))
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

/// `daemon.prefer_own`, or the `SMOOTH_ALLOW_SECOND_DAEMON` escape hatch, which
/// implies it.
#[must_use]
pub fn prefer_own_daemon() -> bool {
    crate::settings::get_bool("daemon.prefer_own") || std::env::var("SMOOTH_ALLOW_SECOND_DAEMON").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// [`decide`] for this machine.
#[must_use]
pub fn owner() -> DaemonOwner {
    decide(&SystemProbe, prefer_own_daemon())
}

/// Launch the app bundle (`open`), which starts its daemon.
///
/// # Errors
/// `open` could not be run or reported failure.
pub fn launch_app(bundle: &Path) -> std::io::Result<()> {
    let status = Command::new("/usr/bin/open").arg(bundle).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("`open {}` exited with {status}", bundle.display())))
    }
}

/// Where the user gets a newer daemon from.
#[must_use]
pub const fn update_hint(owner: &DaemonOwner) -> &'static str {
    match owner {
        DaemonOwner::App { .. } => "update Big Smooth from its app menu",
        DaemonOwner::Cli => "run `brew upgrade th`, then `th down && th up`",
    }
}

/// The message a client shows when the daemon lacks `cap`.
#[must_use]
pub fn missing_capability_message(caps: &DaemonCapabilities, cap: &str, owner: &DaemonOwner) -> String {
    let (feature, since) = capability(cap).map_or((cap, "a newer version"), |c| (c.feature, c.since));
    format!(
        "{feature} needs Big Smooth {since} or newer, but the running daemon is {}. To get it, {}. Everything else keeps working.",
        caps.version_label(),
        update_hint(owner)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    struct FakeProbe {
        bundle: Option<PathBuf>,
        running: bool,
    }

    impl AppProbe for FakeProbe {
        fn app_bundle(&self) -> Option<PathBuf> {
            self.bundle.clone()
        }
        fn app_running(&self) -> bool {
            self.running
        }
    }

    fn app() -> PathBuf {
        PathBuf::from("/Applications/Big Smooth.app")
    }

    #[test]
    fn app_installed_and_running_owns_the_daemon() {
        let probe = FakeProbe {
            bundle: Some(app()),
            running: true,
        };
        assert_eq!(decide(&probe, false), DaemonOwner::App { bundle: app(), running: true });
    }

    #[test]
    fn app_installed_but_not_running_still_owns_it_so_th_launches_it() {
        let probe = FakeProbe {
            bundle: Some(app()),
            running: false,
        };
        assert_eq!(decide(&probe, false), DaemonOwner::App { bundle: app(), running: false });
    }

    #[test]
    fn no_app_means_the_cli_runs_its_own_daemon() {
        let probe = FakeProbe { bundle: None, running: false };
        assert_eq!(decide(&probe, false), DaemonOwner::Cli);
    }

    #[test]
    fn prefer_own_overrides_an_installed_app() {
        for running in [true, false] {
            let probe = FakeProbe { bundle: Some(app()), running };
            assert_eq!(decide(&probe, true), DaemonOwner::Cli, "running={running}");
        }
    }

    #[test]
    fn capabilities_round_trip_and_feature_detect() {
        let caps = DaemonCapabilities::current("0.73.0");
        let json = serde_json::to_string(&caps).unwrap();
        let back: DaemonCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
        assert!(back.has(CAP_SESSION_WORKSPACES));
        assert!(!back.has("not.a.capability"));
        // A body with neither field (an unexpected shape) is "no capabilities".
        let empty: DaemonCapabilities = serde_json::from_str("{}").unwrap();
        assert!(!empty.has(CAP_SESSION_WORKSPACES));
    }

    #[test]
    fn every_capability_is_described_once() {
        for c in CAPABILITIES {
            assert_eq!(CAPABILITIES.iter().filter(|o| o.name == c.name).count(), 1, "{}", c.name);
            assert!(!c.since.is_empty() && !c.feature.is_empty(), "{}", c.name);
        }
    }

    #[test]
    fn missing_capability_message_names_version_and_update_path() {
        let old = DaemonCapabilities::default();
        let app_owner = DaemonOwner::App { bundle: app(), running: true };
        let msg = missing_capability_message(&old, CAP_SESSION_WORKSPACES, &app_owner);
        assert!(msg.contains("/workspace add"), "{msg}");
        assert!(msg.contains("0.73.0 or newer"), "{msg}");
        assert!(msg.contains("predates capability reporting"), "{msg}");
        assert!(msg.contains("app menu"), "{msg}");

        let known = DaemonCapabilities {
            version: "0.72.1".into(),
            capabilities: vec![],
        };
        let msg = missing_capability_message(&known, CAP_SESSION_WORKSPACES, &DaemonOwner::Cli);
        assert!(msg.contains("daemon is 0.72.1"), "{msg}");
        assert!(msg.contains("brew upgrade th"), "{msg}");
    }
}
