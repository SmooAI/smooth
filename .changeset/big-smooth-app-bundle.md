---
'@smooai/smooth': patch
---

Package the daemon as `Big Smooth.app` (macOS privacy foundation)

A bare CLI binary can't declare Info.plist usage-description strings, so it can only get silent `EPERM` on TCC-gated resources and can't request Calendar/EventKit, Contacts, Reminders, etc. at all. This packages `smooth-daemon` as a proper signed `Big Smooth.app` bundle so macOS shows a native "Big Smooth wants to access…" prompt on first access — the prerequisite for Full Disk Access working cleanly *and* for the upcoming ical/Calendar tool.

- `scripts/macos/make-app-bundle.sh` + `scripts/macos/Info.plist`: a generic, reusable bundle builder (assemble → fill version → sign the bundle → verify). `CFBundleName`/`DisplayName` = "Big Smooth" so the prompt is branded; `CFBundleIdentifier` = `ai.smoo.smooth-daemon` (the stable TCC key); `LSUIElement` background app; usage strings for removable volumes (the FDA blocker) and Calendar (imminent). Reusable by a future user-facing installer, not just smoo-hub.
- `scripts/smoo-hub/deploy.sh` now builds + ships `Big Smooth.app` to `~/Applications` (instead of a bare binary), and the launchd plist runs the bundle's executable. The stable-signing work carries over unchanged; a grant made to the earlier bare binary still applies (same identifier + cert = same designated requirement).

Phase 1 of the "Big Smooth as a local menu-bar agent" direction; the menu-bar item (th-f7cb98) is Phase 2.
