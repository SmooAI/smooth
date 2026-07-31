//! macOS menu-bar presence for Big Smooth (pearl th-f7cb98).
//!
//! The OpenClaw-style local-agent UX: when Big Smooth runs on a user's own Mac
//! (as `Big Smooth.app`), it puts a status item in the menu bar so the agent is
//! one click away — **Open Big Smooth** (the web UI) and **Quit**.
//!
//! ## Threading
//! AppKit demands the main thread. So in menu-bar mode the tokio server runs on
//! a background thread and the `NSApplication` run loop owns the main thread.
//! This module is the ONLY place that changes; the headless path (CI, tests,
//! `th daemon`, a launchd agent without `SMOOTH_MENUBAR`) is byte-for-byte
//! unchanged — [`enabled`] gates all of it.
//!
//! ## Opt-in
//! Gated on `SMOOTH_MENUBAR` (truthy) for now, NOT auto-enabled for `.app`
//! launches yet — so shipping the bundle can't flip a live headless daemon into
//! an unvalidated GUI mode. Flip to auto-detect once validated on a real screen.

#![cfg(target_os = "macos")]

use std::future::Future;
use std::process::ExitCode;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AllocAnyThread};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{ns_string, MainThreadMarker, NSString};

/// The web-UI URL the "Open Big Smooth" item launches. Set once in [`run`]
/// before the run loop starts, read by the menu action (which can't easily
/// carry Rust state across the ObjC boundary).
static WEB_URL: OnceLock<String> = OnceLock::new();

/// Whether the menu bar should run for this process. macOS + `SMOOTH_MENUBAR`
/// truthy. Deliberately conservative — see the module docs.
#[must_use]
pub fn enabled() -> bool {
    std::env::var("SMOOTH_MENUBAR").is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        v == "1" || v == "true" || v == "yes" || v == "on"
    })
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

    // Server on a background thread with its own multi-thread runtime — the same
    // runtime shape `#[tokio::main]` builds, just not on the main thread.
    std::thread::Builder::new()
        .name("big-smooth-server".into())
        .spawn(move || match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => {
                if let Err(e) = rt.block_on(server) {
                    tracing::error!(error = %e, "smooth-daemon server exited with error");
                    eprintln!("smooth-daemon: {e:#}");
                    std::process::exit(1);
                }
                // Server returned cleanly (unusual for an always-on daemon).
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("smooth-daemon: failed to build runtime: {e}");
                std::process::exit(1);
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
        // Title-only for v1 (a template icon is a follow-up once validated).
        button.setTitle(ns_string!("Big Smooth"));
    }

    let menu = NSMenu::new(mtm);
    add_item(&menu, mtm, ns_string!("Open Big Smooth"), sel!(openApp:), &target);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(&menu, mtm, ns_string!("Quit Big Smooth"), sel!(quit:), &target);
    status_item.setMenu(Some(&menu));

    // Keep the status item + target alive for the process lifetime.
    std::mem::forget(status_item);
    std::mem::forget(target);

    app.run();
    ExitCode::SUCCESS
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
    fn enabled_reads_truthy_env() {
        // ponytail: single test touching this process-global env; no lock needed.
        for (val, want) in [("1", true), ("true", true), ("YES", true), ("on", true), ("0", false), ("", false)] {
            std::env::set_var("SMOOTH_MENUBAR", val);
            assert_eq!(enabled(), want, "SMOOTH_MENUBAR={val:?}");
        }
        std::env::remove_var("SMOOTH_MENUBAR");
        assert!(!enabled(), "unset → disabled");
    }
}
