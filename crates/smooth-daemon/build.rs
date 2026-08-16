//! Emit `SMOOTH_DAEMON_VERSION` = crate version + short git sha at compile time.
//!
//! Mirrors `smooth-cli/build.rs`'s `TH_VERSION`. The daemon binary is bundled
//! into the desktop app and hot-deployed to always-on boxes (marvin, smoo-hub),
//! so a version *number* alone can't tell "0.35.8 from today" from a stale
//! 0.35.8 the release cache served. Baking the commit lets `smooth-daemon
//! --version` prove which code is actually running — and lets the desktop-publish
//! workflow FAIL if the bundled binary's commit != HEAD (th-76a353: the OTA has
//! shipped stale daemons whose version number lied).
//!
//! Empty sha (non-git build) → just the version, so dev/tarball builds still work.

use std::process::Command;

fn main() {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let version_string = git_sha.map_or_else(|| pkg_version.to_string(), |sha| format!("{pkg_version} ({sha})"));
    println!("cargo:rustc-env=SMOOTH_DAEMON_VERSION={version_string}");

    // Re-stamp when HEAD moves so `--version` stays accurate without a full clean
    // build (also what makes the release git-rev guard trustworthy).
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
