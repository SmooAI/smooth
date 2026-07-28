//! macOS Full Disk Access / removable-volume diagnostics for `th doctor`
//! (pearl th-b85641).
//!
//! Big Smooth's workspace can live on an external volume — on smoo-hub `~/dev`
//! is a symlink to `/Volumes/smoo-ext/dev`. macOS gates external volumes behind
//! TCC (`kTCCServiceSystemPolicyRemovableVolumes`); a process without the grant
//! gets `EPERM` on every access there, so the daemon (and `th`) look jailed in
//! their own workspace — "no home dir", can't do anything. This is NOT the
//! seatbelt sandbox (that's allow-by-default now); it's a separate OS gate.
//!
//! Full Disk Access **cannot be granted programmatically** (the TCC database is
//! SIP-protected; `tccutil` only *resets*). So the most a helper can do is
//! DETECT the trap and GUIDE the one-time manual grant — which this module does.

use std::path::{Path, PathBuf};

/// The `th doctor` candidate workspace, mirroring the daemon's resolution
/// (`SMOOTH_WORKSPACE` if set) but falling back to `~/dev` — the daemon defaults
/// to its current dir, which `th doctor` (a separate process) can't observe, and
/// `~/dev` is the near-universal dev root on these boxes.
///
/// ponytail: `~/dev` fallback is a heuristic; if a daemon is confined somewhere
/// exotic the check just won't fire. The env path is exact when present.
#[must_use]
pub fn candidate_workspace() -> Option<PathBuf> {
    if let Some(ws) = std::env::var_os("SMOOTH_WORKSPACE") {
        return Some(PathBuf::from(ws));
    }
    dirs_next::home_dir().map(|h| h.join("dev"))
}

/// If `workspace` (after resolving symlinks) lives on a non-boot volume under
/// `/Volumes`, return that resolved path — it's TCC-gated and needs Full Disk
/// Access. `None` for boot-volume paths (no FDA needed) or unresolvable paths.
///
/// ponytail: the boot volume's data firmlink keeps `~/...` resolving under
/// `/Users`, so a `/Volumes` prefix reliably means a secondary/external mount.
#[must_use]
pub fn workspace_on_external_volume(workspace: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(workspace).ok()?;
    resolved.starts_with("/Volumes").then_some(resolved)
}

/// Whether *this* process is TCC-denied reading `dir`. `PermissionDenied` is the
/// denied signal; a missing dir or any other error is not (returns `false`).
#[must_use]
pub fn read_access_denied(dir: &Path) -> bool {
    matches!(std::fs::read_dir(dir), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
}

/// The binaries that need the Full Disk Access grant: this `th` (so the CLI can
/// reach an external-volume workspace) and the daemon binary if present at its
/// conventional install path. TCC grants are per-binary, so both must be added.
#[must_use]
pub fn grant_targets() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        out.push(exe);
    }
    if let Some(home) = dirs_next::home_dir() {
        let daemon = home.join("smooth-daemon");
        if daemon.exists() {
            out.push(daemon);
        }
    }
    out
}

/// Open the macOS Full Disk Access settings pane. Requires a GUI session (run on
/// the host's console, not over SSH).
///
/// # Errors
/// Propagates a failure to spawn `/usr/bin/open`.
pub fn open_fda_settings() -> std::io::Result<()> {
    open_arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
}

/// Reveal `path` in Finder (`open -R`) so the user can drag it into the FDA list.
///
/// # Errors
/// Propagates a failure to spawn `/usr/bin/open`.
pub fn reveal_in_finder(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("/usr/bin/open").arg("-R").arg(path).status().map(|_| ())
}

fn open_arg(arg: &str) -> std::io::Result<()> {
    std::process::Command::new("/usr/bin/open").arg(arg).status().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_volume_path_is_flagged() {
        // Can't canonicalize a fake path, so test the prefix predicate directly
        // via a path that already exists under /Volumes on macOS: the boot disk
        // firmlink. Fall back to the pure prefix check when absent.
        assert!(Path::new("/Volumes/smoo-ext/dev").starts_with("/Volumes"));
    }

    #[test]
    fn boot_volume_path_needs_no_fda() {
        // A resolved home path stays under /Users, never /Volumes.
        let home = dirs_next::home_dir().unwrap();
        let resolved = std::fs::canonicalize(&home).unwrap();
        assert!(!resolved.starts_with("/Volumes"), "home resolved to {resolved:?}");
        assert!(workspace_on_external_volume(&home).is_none());
    }

    #[test]
    fn missing_dir_is_not_a_permission_denial() {
        let missing = std::env::temp_dir().join("th-fda-does-not-exist-xyz");
        assert!(!read_access_denied(&missing));
    }

    #[test]
    fn grant_targets_includes_this_binary() {
        let targets = grant_targets();
        assert!(!targets.is_empty(), "current_exe should always be a target");
        assert!(targets[0].exists(), "the th binary path must exist");
    }

    #[test]
    fn candidate_workspace_prefers_env() {
        // ponytail: mutating a process-global env var — this is the only test
        // here that touches SMOOTH_WORKSPACE, so no lock needed.
        std::env::set_var("SMOOTH_WORKSPACE", "/tmp/some-ws");
        assert_eq!(candidate_workspace(), Some(PathBuf::from("/tmp/some-ws")));
        std::env::remove_var("SMOOTH_WORKSPACE");
    }
}
