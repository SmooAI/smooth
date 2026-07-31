---
'@smooai/smooth': patch
---

Big Smooth menu-bar item (macOS) — Phase 2 of the local-agent app

When Big Smooth runs on a user's own Mac it now puts a status item in the menu bar (**Open Big Smooth** → the web UI, **Quit**), the OpenClaw-style local-agent UX. `smooth-daemon`'s `main()` is restructured so the tokio server runs on a background thread while the AppKit run loop owns the main thread; the headless path (CI, tests, `th daemon`, a launchd agent) is unchanged and gated behind `SMOOTH_MENUBAR`.

The AppKit/objc2 FFI (which needs `unsafe`) is quarantined in a new `smooai-smooth-menubar` crate — the one crate that opts out of the workspace-wide `unsafe_code = "forbid"`, keeping `forbid` everywhere else. On non-macOS the crate is empty and the daemon doesn't depend on it. Deliberately opt-in (not auto-enabled for `.app` launches yet) so shipping the bundle can't flip a live headless daemon into a GUI mode before it's validated on a real screen; v1 is title-only (icon/status/restart/logs are follow-ups).
