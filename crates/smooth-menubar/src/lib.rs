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
pub mod setup;

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

/// Whether the menu bar should run for this process: `SMOOTH_MENUBAR` decides
/// when it's set either way, otherwise the daemon presents a menu bar iff it was
/// launched as a `.app` bundle (double-clicked / `open`ed / a login-item) — the
/// natural product signal. A plain `smooth-daemon` on `$PATH` (CLI, tests, a bare
/// launchd agent) stays headless.
///
/// The **off** direction is what the Electron desktop app uses: it bundles the
/// daemon in its own `Contents/MacOS` (so EventKit finds the app's usage strings)
/// and owns the tray itself, so the daemon must not raise a second status item.
#[must_use]
pub fn enabled() -> bool {
    env_override().unwrap_or_else(|| std::env::current_exe().is_ok_and(|p| launched_from_app_bundle(&p)))
}

/// The `SMOOTH_MENUBAR` override: `Some(true)` forces the menu bar on for a CLI
/// run (validating without packaging an app), `Some(false)` forces it off inside
/// a bundle, `None` when unset or unparseable.
fn env_override() -> Option<bool> {
    let raw = std::env::var("SMOOTH_MENUBAR").ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
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

/// The System Settings deep link for Privacy & Security → Full Disk Access —
/// the grant that unlocks `~/Library/Messages/chat.db` and external-volume
/// workspaces. FDA cannot be granted programmatically (SIP-protected TCC db),
/// so getting the user to the toggle is the whole job. Same URL as
/// `smooth-cli`'s `fda::open_fda_settings`, duplicated rather than depending on
/// the CLI crate from this leaf.
const FDA_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// An Apple Event to Messages that sends nothing but still trips the Automation
/// prompt. Same probe as `smooth-cli`'s `imessage_setup`; here it fires from
/// inside Big Smooth.app, so TCC attributes the grant to the app rather than to
/// `th` — which is the whole reason these items exist in the menu.
const MESSAGES_PROBE: &str = r#"tell application "Messages" to get name"#;

/// `open(1)` a URL. Used for both the web UI and the System Settings deep links.
fn open_url(url: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}

/// Ask for an EventKit grant off the main thread and report the outcome.
///
/// Off-thread is load-bearing: `request_*_access` blocks until the user answers
/// the prompt, and blocking the AppKit main thread would deadlock the prompt it
/// is waiting for. Already-answered grants return immediately — macOS never
/// re-prompts, so a `denied` here has to be undone in System Settings.
fn grant_in_background(what: &'static str, request: fn() -> eventkit::Access) {
    std::thread::spawn(move || {
        let access = request();
        tracing::info!(status = access.label(), "{what} access request answered");
        let message = if access.granted() {
            format!("{what} access: granted.")
        } else {
            format!(
                "{what} access: {}.\n\nEnable it in System Settings → Privacy & Security → {what}.",
                access.label()
            )
        };
        notify(&message);
    });
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
                open_url(url);
            }
        }

        #[unsafe(method(openFullDiskAccess:))]
        fn open_full_disk_access(&self, _sender: Option<&AnyObject>) {
            open_url(FDA_SETTINGS_URL);
        }

        #[unsafe(method(grantCalendar:))]
        fn grant_calendar(&self, _sender: Option<&AnyObject>) {
            grant_in_background("Calendar", eventkit::request_calendar_access);
        }

        #[unsafe(method(grantReminders:))]
        fn grant_reminders(&self, _sender: Option<&AnyObject>) {
            grant_in_background("Reminders", eventkit::request_reminders_access);
        }

        #[unsafe(method(setUpMessages:))]
        fn set_up_messages(&self, _sender: Option<&AnyObject>) {
            // Two grants, two mechanisms: chat.db reads need Full Disk Access
            // (manual toggle, no prompt exists), sending needs Automation
            // (promptable — the probe below is what makes it appear).
            open_url(FDA_SETTINGS_URL);
            std::thread::spawn(|| {
                match std::process::Command::new("/usr/bin/osascript").arg("-e").arg(MESSAGES_PROBE).output() {
                    Ok(out) if out.status.success() => notify("Messages automation: allowed — Big Smooth can send texts.\n\nFor reading, add Big Smooth to Full Disk Access in the window that just opened."),
                    Ok(out) => notify(&format!(
                        "Messages automation: not granted.\n\n{}\n\nAllow it in System Settings → Privacy & Security → Automation.",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )),
                    Err(e) => notify(&format!("Couldn't run the Messages check: {e}")),
                }
            });
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
    // The macOS access grants, driven from inside the app so TCC attributes them
    // to Big Smooth.app — the process that actually reads chat.db and calls
    // EventKit — instead of to whatever `th` happened to run (pearl th-ba764e).
    // A submenu rather than four more top-level rows: one-time setup shouldn't
    // outweigh the two things you click every day.
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let setup = NSMenu::new(mtm);
    add_item(&setup, mtm, ns_string!("Configure Full Disk Access…"), sel!(openFullDiskAccess:), &target);
    add_item(&setup, mtm, ns_string!("Grant Calendar Access…"), sel!(grantCalendar:), &target);
    add_item(&setup, mtm, ns_string!("Grant Reminders Access…"), sel!(grantReminders:), &target);
    add_item(&setup, mtm, ns_string!("Set Up Messages…"), sel!(setUpMessages:), &target);
    let setup_item = NSMenuItem::new(mtm);
    setup_item.setTitle(ns_string!("Set Up"));
    setup_item.setSubmenu(Some(&setup));
    menu.addItem(&setup_item);

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
    fn env_override_decides_in_both_directions() {
        // ponytail: single test touching this process-global env; no lock needed.
        let cases = [
            ("1", Some(true)),
            ("true", Some(true)),
            ("YES", Some(true)),
            ("on", Some(true)),
            ("0", Some(false)),
            ("off", Some(false)),
            ("NO", Some(false)),
            ("", None),
            ("maybe", None),
        ];
        for (val, want) in cases {
            std::env::set_var("SMOOTH_MENUBAR", val);
            assert_eq!(env_override(), want, "SMOOTH_MENUBAR={val:?}");
        }
        std::env::remove_var("SMOOTH_MENUBAR");
        assert_eq!(env_override(), None, "unset → no opinion");

        // The off direction must beat the in-a-bundle default — that's what lets
        // the Electron app bundle the daemon without getting a second status item.
        std::env::set_var("SMOOTH_MENUBAR", "0");
        assert!(!enabled(), "explicit off wins over the bundle heuristic");
        std::env::remove_var("SMOOTH_MENUBAR");
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
    fn the_messages_probe_sends_nothing() {
        // Load-bearing: a setup click must never text a human. If someone
        // "improves" this into a real send, this fails.
        assert!(!MESSAGES_PROBE.contains("send"), "{MESSAGES_PROBE}");
        assert!(!MESSAGES_PROBE.contains("participant"), "{MESSAGES_PROBE}");
        assert!(MESSAGES_PROBE.contains("get name"));
    }

    #[test]
    fn fda_link_targets_the_full_disk_access_pane() {
        // Drift here silently opens the wrong Settings pane; the anchor is what
        // selects Full Disk Access rather than the top of Privacy & Security.
        assert!(FDA_SETTINGS_URL.starts_with("x-apple.systempreferences:"));
        assert!(FDA_SETTINGS_URL.ends_with("?Privacy_AllFiles"));
    }

    #[test]
    fn install_cli_errors_when_nothing_is_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let th = tmp.path().join("th");
        std::fs::write(&th, b"#!/bin/sh\n").unwrap();
        assert!(install_cli(&th, &[PathBuf::from("/nonexistent-th-a647da/bin")]).is_err());
    }
}
