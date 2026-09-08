---
'@smooai/smooth': minor
---

SmoothFlow macOS shell (`apps/smoothflow`, pearl th-f7f823): a native AppKit
fleet console for Smooth agents — sidebar grouped by project, one libghostty
surface per session fed by `flow.output`, inbox with approve/deny, usage-limit
and held-session cards, steer bar, fan-out sheet, pearl rail, notifications
keyed on `flow.attention`. The app spawns `smooth-daemon` and the `smoothflow`
tmux server itself so every TCC grant (Calendar, Reminders, FDA, Notifications,
Apple Events) attributes to the app and is inherited by the daemon and agents
— measured and written down in `docs/Architecture/SmoothFlow-macOS.md`.
Builds against a zero-dependency mock flow engine (`mock/server.mjs`) until
the engine lands. New `smoothflow-mac` workflow: compile + XCTest on PR,
signed/notarized DMG on dispatch.
