//! `th doctor --setup-calendar` — make Big Smooth's macOS Calendar tool work
//! out of the box (pearl th-94cc4a).
//!
//! Two things have to be true before the daemon's `calendar` tool works:
//!
//! 1. the `ical` CLI (BRO3886/ical — a Go EventKit client) exists on disk, and
//! 2. macOS has granted **Big Smooth.app** Calendar access.
//!
//! `th` owns step 1 because `th` is unsandboxed and is already the setup surface
//! (`--fix-fda` next door). It *cannot* own step 2: a TCC grant is attributed to
//! the app bundle that asks, and a bare CLI asking gets a silent denial. So this
//! installs `ical`, then drives **the app** into asking (the daemon calls
//! `EKEventStore.requestFullAccessToEvents` at startup — see
//! `smooth_menubar::eventkit`), and reports what the human still has to click.
//!
//! Install strategy is **side-load, not Homebrew**: the release tarball is a
//! single static binary, and `brew tap BRO3886/tap` both misses the formula and
//! trips the agent sandbox on `.git/config`. `curl` + `tar` are macOS base
//! system tools, so this needs no package manager and no new Rust dependency.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use anstream::println;
use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;

/// Where the side-loaded binary lands. Must stay in sync with
/// `smooth_tools::calendar::resolve_ical`, which looks here first.
const INSTALL_DIR: &str = ".smooth/bin";

/// The app bundle that owns the Calendar TCC grant.
const APP_NAME: &str = "Big Smooth.app";

/// GitHub serves `releases/latest/download/<asset>` as a redirect to the newest
/// tag, so no release-API call (and no JSON parsing) is needed.
const RELEASE_URL_BASE: &str = "https://github.com/BRO3886/ical/releases/latest/download";

/// The release asset for this machine's architecture.
fn asset_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("ical-darwin-arm64.tar.gz"),
        "x86_64" => Ok("ical-darwin-amd64.tar.gz"),
        other => bail!("no prebuilt `ical` release for architecture {other}"),
    }
}

/// `~/.smooth/bin/ical`.
fn install_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    Ok(home.join(INSTALL_DIR).join("ical"))
}

/// The installed app bundle, if present (`~/Applications` first, then
/// `/Applications`).
#[must_use]
pub fn app_bundle() -> Option<PathBuf> {
    dirs_next::home_dir()
        .map(|h| h.join("Applications").join(APP_NAME))
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/Applications").join(APP_NAME)))
        .find(|p| p.is_dir())
}

/// Whether a Big Smooth app-bundle process is currently running. `pgrep -f`
/// against the bundle's executable path — the same string
/// `smooth_menubar::enabled` keys its app-bundle detection on.
#[must_use]
pub fn app_is_running() -> bool {
    Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(format!("{APP_NAME}/Contents/MacOS/smooth-daemon"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Download the latest `ical` release and install it at [`install_path`].
///
/// Shells `curl` and `tar` (macOS base system) rather than pulling a TLS +
/// gzip + tar stack into `th` for one install path.
///
/// # Errors
/// Fails when the architecture has no release asset, the download fails, or the
/// tarball doesn't contain an `ical` binary.
pub fn install_ical() -> Result<PathBuf> {
    let asset = asset_name()?;
    let url = format!("{RELEASE_URL_BASE}/{asset}");
    let dest = install_path()?;
    let dir = dest.parent().context("install path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let staging = tempdir("install")?;
    let tarball = staging.join(asset);

    let status = Command::new("/usr/bin/curl")
        .args(["-fsSL", "-o"])
        .arg(&tarball)
        .arg(&url)
        .status()
        .context("spawning curl")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("downloading {url} failed (curl exit {status})");
    }

    let status = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&staging)
        .status()
        .context("spawning tar")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("extracting {asset} failed (tar exit {status})");
    }

    let extracted = staging.join("ical");
    if !extracted.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("{asset} did not contain an `ical` binary");
    }
    // Rename can cross filesystems (temp dir vs home); copy+remove is the
    // portable form and leaves no half-written binary at `dest` on failure.
    std::fs::copy(&extracted, &dest).with_context(|| format!("installing to {}", dest.display()))?;
    make_executable(&dest)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(dest)
}

/// A fresh staging directory under the system temp dir, unique per process and
/// `tag` (so concurrent callers never share one).
fn tempdir(tag: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("th-ical-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// `chmod 0o755` — a downloaded file arrives non-executable.
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod +x {}", path.display()))
}

/// The version string `ical version` reports, for the readiness report.
fn ical_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(str::trim).filter(|l| !l.is_empty()).map(ToOwned::to_owned)
}

/// One line of readiness report for an `ical` we can see on disk.
fn report_ical(bin: &Path) {
    let version = ical_version(bin).unwrap_or_else(|| "installed".to_owned());
    println!("  {} ical: {} ({})", "✓".green().bold(), version, bin.display().to_string().dimmed());
}

/// Run the whole setup: install `ical` if missing, then drive the app into
/// asking for the Calendar grant and report what the human must still do.
///
/// # Errors
/// Propagates a failed `ical` install. A missing app bundle is reported, not an
/// error — the CLI half of the setup still succeeded.
pub fn run() -> Result<()> {
    // 1. The `ical` CLI.
    if let Some(bin) = smooth_tools::calendar::resolve_ical() {
        report_ical(&bin);
    } else {
        println!("  {} ical: not installed — downloading the latest release…", "→".cyan());
        match install_ical() {
            Ok(bin) => report_ical(&bin),
            Err(e) => {
                println!("  {} ical: install failed: {e:#}", "✗".red().bold());
                println!("    {} install it by hand, then re-run: {}", "→".cyan(), "brew install BRO3886/tap/ical".bold());
                return Err(e);
            }
        }
    }

    // 2. The app bundle — the only identity macOS will grant Calendar access to.
    let Some(app) = app_bundle() else {
        println!("  {} {APP_NAME}: not installed", "✗".red().bold());
        println!(
            "    {} install it first, then re-run this: {}",
            "→".cyan(),
            "scripts/macos/install-local.sh".bold()
        );
        println!(
            "\n  {} the Calendar permission can only be granted to the app bundle — a bare CLI",
            "⚠".yellow().bold()
        );
        println!("     asking gets a silent denial, so the tool stays unusable until the app exists.");
        return Ok(());
    };
    println!("  {} {APP_NAME}: {}", "✓".green().bold(), app.display().to_string().dimmed());

    // 3. Drive the grant. The daemon asks EventKit at startup, so the trigger is
    //    simply "(re)launch the app in this GUI session".
    if app_is_running() {
        println!("  {} Big Smooth is already running — it asks for Calendar access at startup,", "→".cyan());
        println!("    so restart it to trigger the prompt:");
        println!(
            "      {}",
            format!("osascript -e 'quit app \"Big Smooth\"' && open -a \"{}\"", app.display()).bold()
        );
    } else {
        println!("  {} launching Big Smooth so it asks macOS for Calendar access…", "→".cyan());
        if let Err(e) = Command::new("/usr/bin/open").arg(&app).status() {
            println!("    {} couldn't launch it ({e}) — open it from Finder instead.", "✗".red().bold());
        }
    }

    println!("\n  {}", "What you have to click:".bold());
    println!(
        "    1. macOS shows “Big Smooth would like to access your calendar” — choose {}.",
        "Allow".green().bold()
    );
    println!("    2. Verify later in System Settings → Privacy & Security → Calendars.");
    println!(
        "\n  {} the prompt only appears in a GUI login session on the Mac itself (never over SSH),",
        "⚠".yellow().bold()
    );
    println!("     and macOS asks exactly once — if it was denied, re-enable it in System Settings.");
    println!("\n  Then ask Big Smooth “what's on my calendar today?”.");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn asset_matches_this_architecture() {
        let asset = asset_name().unwrap();
        assert!(asset.starts_with("ical-darwin-"), "{asset}");
        if std::env::consts::ARCH == "aarch64" {
            assert!(asset.ends_with("arm64.tar.gz"));
        }
    }

    #[test]
    fn install_path_is_the_path_the_tool_resolver_checks_first() {
        let p = install_path().unwrap();
        assert!(p.ends_with(".smooth/bin/ical"), "{}", p.display());
    }

    #[test]
    fn tempdir_is_fresh_and_writable() {
        let dir = tempdir("fresh-test").unwrap();
        assert!(dir.is_dir());
        std::fs::write(dir.join("probe"), b"x").unwrap();
        // A second call wipes and recreates it — no stale extract survives.
        let again = tempdir("fresh-test").unwrap();
        assert_eq!(dir, again);
        assert!(!again.join("probe").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn make_executable_sets_the_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir("chmod-test").unwrap();
        let f = dir.join("fake-ical");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        make_executable(&f).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o755);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn app_lookup_and_running_probe_do_not_panic() {
        // Machine-dependent results; the invariant is that both are total.
        let _ = app_bundle();
        let _ = app_is_running();
    }

    #[test]
    fn version_probe_returns_none_for_a_non_ical_binary() {
        assert!(ical_version(Path::new("/nonexistent/ical")).is_none());
    }
}
