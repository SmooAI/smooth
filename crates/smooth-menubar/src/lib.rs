//! macOS menu-bar presence for Big Smooth (pearl th-f7cb98).
//!
//! The OpenClaw-style local-agent UX: when Big Smooth runs on a user's own Mac
//! (as `Big Smooth.app`), it puts a status item in the menu bar so the agent is
//! one click away — **Open Big Smooth** (the web UI), **Install th CLI…** (when
//! the app bundles one, pearl th-a647da), and **Quit**.
//!
//! ## Threading
//! AppKit demands the main thread. So in menu-bar mode the tokio server runs on
//! a background thread and the `NSApplication` run loop owns the main thread.
//! This module is the ONLY place that changes; the headless path (CI, tests,
//! `th daemon`, a launchd agent without `SMOOTH_MENUBAR`) is byte-for-byte
//! unchanged — [`enabled`] gates all of it.
//!
//! ## When it turns on
//! Either the `SMOOTH_MENUBAR` env opt-in, OR the daemon was launched as a
//! `.app` bundle (double-clicked / `open`ed / a login-item) — the natural
//! product signal. A plain `smooth-daemon` on `$PATH` (CLI, tests, a bare
//! launchd agent like smoo-hub's) stays headless.

#![cfg(target_os = "macos")]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AllocAnyThread};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{ns_string, MainThreadMarker, NSData, NSSize, NSString};

pub mod eventkit;
pub mod reminders;

/// The web-UI URL the "Open Big Smooth" item launches. Set once in [`run`]
/// before the run loop starts, read by the menu action (which can't easily
/// carry Rust state across the ObjC boundary).
static WEB_URL: OnceLock<String> = OnceLock::new();

/// The `th` binary bundled inside the .app, if this process was launched from
/// one. Same story as [`WEB_URL`]: set before the run loop, read by the
/// "Install th CLI…" action.
static BUNDLED_TH: OnceLock<PathBuf> = OnceLock::new();

/// Where "Install th CLI…" tries to put the symlink, in order. `/usr/local/bin`
/// is the one already on everyone's `PATH`; `~/.local/bin` is the fallback for
/// Macs where it isn't writable (Apple Silicon without Homebrew, mostly).
fn link_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/local/bin")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    dirs
}

/// Whether the menu bar should run for this process: either the `SMOOTH_MENUBAR`
/// env opt-in, OR the daemon was launched as a `.app` bundle (double-clicked /
/// `open`ed / a login-item) — the natural product signal. A plain
/// `smooth-daemon` on `$PATH` (CLI, tests, a bare launchd agent) stays headless.
#[must_use]
pub fn enabled() -> bool {
    env_opt_in() || std::env::current_exe().is_ok_and(|p| launched_from_app_bundle(&p))
}

/// The `SMOOTH_MENUBAR` env override (truthy). Also forces the menu bar on for a
/// CLI run, for validating without packaging an app.
fn env_opt_in() -> bool {
    std::env::var("SMOOTH_MENUBAR").is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// True when `exe` lives inside a macOS `.app` bundle
/// (`…/Big Smooth.app/Contents/MacOS/smooth-daemon`) — i.e. it was launched as an
/// app, so it should present its menu bar.
fn launched_from_app_bundle(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// The `th` shipped inside the bundle, resolved from this executable:
/// `…/Big Smooth.app/Contents/MacOS/smooth-daemon` → `…/Contents/Resources/bin/th`
/// (see scripts/macos/make-app-bundle.sh). `None` when it isn't there — an
/// unbundled run, or a build made without the CLI.
fn bundled_th(exe: &Path) -> Option<PathBuf> {
    let th = exe.parent()?.parent()?.join("Resources/bin/th");
    th.is_file().then_some(th)
}

/// Symlink `th` onto the user's `PATH`, trying `dirs` in order (the VS Code
/// "install 'code' command" pattern). Returns the link that was created.
fn install_cli(th: &Path, dirs: &[PathBuf]) -> anyhow::Result<PathBuf> {
    for dir in dirs {
        if std::fs::create_dir_all(dir).is_err() {
            continue; // not writable (the usual /usr/local/bin case) — next candidate
        }
        let link = dir.join("th");
        // Replace whatever is there: an older symlink, or a hand-installed copy.
        // `symlink_metadata` (not `exists`) so a DANGLING symlink is removed too.
        if std::fs::symlink_metadata(&link).is_ok() && std::fs::remove_file(&link).is_err() {
            continue;
        }
        if std::os::unix::fs::symlink(th, &link).is_ok() {
            return Ok(link);
        }
    }
    anyhow::bail!("couldn't write a `th` symlink into any of: {dirs:?}")
}

/// Show a modal message. `osascript` instead of `NSAlert` on purpose: a menu-bar
/// action shouldn't block the AppKit main thread, and this keeps the unsafe
/// AppKit surface of this crate as small as it already is.
fn notify(message: &str) {
    // AppleScript string literals escape with backslashes, same as Rust's.
    let escaped = message.replace('\\', r"\\").replace('"', "\\\"");
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            &format!(r#"display dialog "{escaped}" buttons {{"OK"}} default button "OK" with title "Big Smooth""#),
        ])
        .spawn();
}

define_class!(
    // A trivial NSObject subclass that carries the menu actions. No ivars — the
    // URL lives in the `WEB_URL` static, and Quit just exits the process.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "BigSmoothMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openApp:))]
        fn open_app(&self, _sender: Option<&AnyObject>) {
            if let Some(url) = WEB_URL.get() {
                let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
            }
        }

        #[unsafe(method(installCli:))]
        fn install_cli_action(&self, _sender: Option<&AnyObject>) {
            let Some(th) = BUNDLED_TH.get() else { return };
            match install_cli(th, &link_dirs()) {
                Ok(link) => notify(&format!("The `th` command is installed at {}.\n\nOpen a new terminal and run `th --help`.", link.display())),
                Err(e) => notify(&format!("Couldn't install the `th` command: {e}")),
            }
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            // The tokio server is on a background thread; exiting the process is
            // the clean stop for a user-driven Quit (launchd KeepAlive, if any,
            // is a separate concern handled by the plist, not the menu).
            std::process::exit(0);
        }
    }
);

impl MenuTarget {
    fn new() -> Retained<Self> {
        unsafe { msg_send![Self::alloc(), init] }
    }
}

/// Run Big Smooth with a menu-bar item: `server` (the long-running daemon
/// future) runs on a background thread while the `NSApplication` run loop owns
/// the main thread. Never returns until Quit (which exits the process).
///
/// `web_url` is what "Open Big Smooth" launches (the local web UI).
#[must_use]
pub fn run<F>(web_url: String, server: F) -> ExitCode
where
    F: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let _ = WEB_URL.set(web_url);

    // Ask for the EventKit TCC grants while we're a bundled GUI app — the only
    // context where the OS will show the prompts (pearl th-94cc4a). Done off the
    // main thread so the AppKit run loop below still comes up; without a grant
    // the `calendar` tool's `ical` child process gets a silent denial, and the
    // `reminders` tool's in-process EventKit calls come back empty. Calendar and
    // Reminders are separate grants, so both are asked for — macOS shows each
    // prompt at most once, ever.
    eventkit::request_calendar_access_in_background();
    eventkit::request_reminders_access_in_background();

    // Server on a background thread with its own multi-thread runtime — the same
    // runtime shape `#[tokio::main]` builds, just not on the main thread.
    std::thread::Builder::new()
        .name("big-smooth-server".into())
        .spawn(move || match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => {
                // Keep the menu bar alive even if the server stops or fails to
                // start (port busy, missing creds) — the app must not silently
                // vanish; the user can still see it and Quit.
                if let Err(e) = rt.block_on(server) {
                    tracing::error!(error = %e, "smooth-daemon server exited with error");
                    eprintln!("smooth-daemon: {e:#}");
                } else {
                    tracing::warn!("smooth-daemon server stopped");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to build tokio runtime");
                eprintln!("smooth-daemon: failed to build runtime: {e}");
            }
        })
        .expect("spawn server thread");

    // Menu bar on the main thread.
    let mtm = MainThreadMarker::new().expect("menubar::run must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: a background/menu-bar app with no Dock icon (matches LSUIElement).
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let target = MenuTarget::new();

    let status_item: Retained<NSStatusItem> = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status_item.button(mtm) {
        // The `th` mark as a template image (tints for light/dark). Fall back to
        // text if the image can't be decoded, so the item is never invisible.
        if let Some(icon) = template_icon() {
            button.setImage(Some(&icon));
        } else {
            button.setTitle(ns_string!("Big Smooth"));
        }
    }

    let menu = NSMenu::new(mtm);
    add_item(&menu, mtm, ns_string!("Open Big Smooth"), sel!(openApp:), &target);
    // Only offered when the .app actually carries a `th` — an unbundled or
    // CLI-less build just gets the shorter menu.
    if let Some(th) = std::env::current_exe().ok().and_then(|exe| bundled_th(&exe)) {
        let _ = BUNDLED_TH.set(th);
        add_item(&menu, mtm, ns_string!("Install th CLI…"), sel!(installCli:), &target);
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(&menu, mtm, ns_string!("Quit Big Smooth"), sel!(quit:), &target);
    status_item.setMenu(Some(&menu));

    // Keep the status item + target alive for the process lifetime.
    std::mem::forget(status_item);
    std::mem::forget(target);

    app.run();
    ExitCode::SUCCESS
}

/// The `th` mark as a menu-bar template image — black shape + alpha, so macOS
/// tints it for the light/dark menu bar. Rendered from `images/smooth-icon.svg`
/// and embedded at build time.
fn template_icon() -> Option<Retained<NSImage>> {
    const PNG: &[u8] = include_bytes!("../assets/th-icon.png");
    let data = NSData::with_bytes(PNG);
    let img = NSImage::initWithData(NSImage::alloc(), &data)?;
    img.setTemplate(true);
    // Point size (the PNG is 2×); 18pt is the conventional menu-bar glyph size.
    img.setSize(NSSize::new(18.0, 18.0));
    Some(img)
}

fn add_item(menu: &NSMenu, mtm: MainThreadMarker, title: &NSString, action: objc2::runtime::Sel, target: &MenuTarget) {
    let item = NSMenuItem::new(mtm);
    item.setTitle(title);
    unsafe {
        item.setTarget(Some(target));
        item.setAction(Some(action));
    }
    menu.addItem(&item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_opt_in_reads_truthy_values() {
        // ponytail: single test touching this process-global env; no lock needed.
        for (val, want) in [("1", true), ("true", true), ("YES", true), ("on", true), ("0", false), ("", false)] {
            std::env::set_var("SMOOTH_MENUBAR", val);
            assert_eq!(env_opt_in(), want, "SMOOTH_MENUBAR={val:?}");
        }
        std::env::remove_var("SMOOTH_MENUBAR");
        assert!(!env_opt_in(), "unset → disabled");
    }

    #[test]
    fn app_bundle_launch_detected_by_path() {
        assert!(launched_from_app_bundle(Path::new(
            "/Users/x/Applications/Big Smooth.app/Contents/MacOS/smooth-daemon"
        )));
        // A plain CLI binary on $PATH is NOT an app launch.
        assert!(!launched_from_app_bundle(Path::new("/Users/x/.cargo/bin/smooth-daemon")));
        assert!(!launched_from_app_bundle(Path::new("/opt/homebrew/bin/smooth-daemon")));
    }

    #[test]
    fn bundled_th_found_only_when_the_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let macos = tmp.path().join("Big Smooth.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("smooth-daemon");

        assert_eq!(bundled_th(&exe), None, "no Resources/bin/th yet");

        let bin = tmp.path().join("Big Smooth.app/Contents/Resources/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("th"), b"#!/bin/sh\n").unwrap();
        assert_eq!(bundled_th(&exe), Some(bin.join("th")));

        // A directory named `th` is not a binary.
        std::fs::remove_file(bin.join("th")).unwrap();
        std::fs::create_dir(bin.join("th")).unwrap();
        assert_eq!(bundled_th(&exe), None);
    }

    #[test]
    fn install_cli_falls_back_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let th = tmp.path().join("th");
        std::fs::write(&th, b"#!/bin/sh\n").unwrap();

        // First candidate is unwritable (can't mkdir under /), so it falls through.
        let good = tmp.path().join("bin");
        let dirs = vec![PathBuf::from("/nonexistent-th-a647da/bin"), good.clone()];

        let link = install_cli(&th, &dirs).unwrap();
        assert_eq!(link, good.join("th"));
        assert_eq!(std::fs::read_link(&link).unwrap(), th);

        // Re-running replaces an existing entry rather than failing…
        assert!(install_cli(&th, &dirs).is_ok());
        // …including a plain file left by a hand-install.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"stale copy").unwrap();
        assert!(install_cli(&th, &dirs).is_ok());
        assert_eq!(std::fs::read_link(&link).unwrap(), th);
    }

    #[test]
    fn install_cli_errors_when_nothing_is_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let th = tmp.path().join("th");
        std::fs::write(&th, b"#!/bin/sh\n").unwrap();
        assert!(install_cli(&th, &[PathBuf::from("/nonexistent-th-a647da/bin")]).is_err());
    }
}
