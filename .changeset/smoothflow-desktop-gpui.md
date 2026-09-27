---
'@smooai/smooth': minor
---

SmoothFlow for Linux and Windows, first cut (th-3e6020): `apps/smoothflow-desktop` is a Rust GPUI app with GPU-rendered terminals. It follows the SmoothFlow Client Spec through `smooth-flow-client`. It finds the flow engine the way every client does, shows the fleet sidebar (grouped by project, with states and counts), and streams the focused session's terminal live: `alacritty_terminal` for VT state, GPUI's GPU text pipeline for drawing, and xterm keyboard input. It's its own Cargo workspace, with a CI workflow that builds and tests it on Linux, Windows and macOS.
