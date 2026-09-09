---
'@smooai/smooth': patch
---

SmoothFlow macOS UI tests (th-a58a97): a `SmoothFlowUITests` XCUITest target that drives the built app against `mock/server.mjs` and against the real flow engine (`flow_e2e_server` + `fake-claude`) — sidebar states, surface + header on select, inbox permission Allow, steer → hooks → idle, hook-reported permission answered from the inbox, Kill & Resume relaunching with `--resume`, settings panes. Accessibility identifiers on the shell's views, `SMOOTHFLOW_UI_TEST=1` keeps Sparkle quiet, and `smoothflow-mac.yml` runs both suites on PR with GhosttyKit cached.
