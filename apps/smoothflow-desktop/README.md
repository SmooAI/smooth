# SmoothFlow Desktop (Linux + Windows)

The SmoothFlow agent fleet console for Linux and Windows, with GPU-rendered
terminals, built on [GPUI](https://www.gpui.rs/) through
[`gpui-kit`](https://crates.io/crates/gpui-kit). Epic th-3e6020.

It implements the [SmoothFlow Client Spec](../../docs/Architecture/SmoothFlow-Client-Spec.md),
the same spec the Mac app (`apps/smoothflow`) and the phones follow. Its rules
come from `crates/smooth-flow-client`, whose conformance vectors every client
replays.

## Status (v0)

- Connects to the flow engine the way every client does: `$SMOOTH_FLOW_ADDR` →
  `~/.smooth/flow.addr` → `~/.smooth/daemon.addr`, with the local token.
  It reconnects with backoff.
- **Fleet sidebar**: grouped by project with shells together, a state dot,
  titles from the shared rules, and the connection and counts footer.
- **Terminal**: the focused session's live PTY stream (`flow.attach` /
  `flow.output`), parsed by `alacritty_terminal` and drawn by GPUI (GPU glyph
  atlas: Vulkan on Linux, DirectX on Windows, Metal on macOS). It has
  Catppuccin Mocha colours, bold, italic, underline and inverse, and
  xterm-style keyboard input. It resizes with the window.

Coming next, in spec order: tabs and splits, New Session (kind picker with
degraded harnesses and the Directory field), approvals, Close Out, the pearl
rail, measured cell metrics, cursor and selection, a bundled Nerd Font, the
copilot pane, and libghostty-vt as the VT backend.

## Build

```bash
cd apps/smoothflow-desktop
cargo run            # needs a running flow engine (SmoothFlow's daemon or `th up`)
cargo test
```

It's its own Cargo workspace, so GPUI never enters the main `th` build. On
Linux it needs the X11/Wayland/Vulkan/fontconfig/D-Bus/PipeWire development
packages; `.github/workflows/smoothflow-desktop.yml` lists them. On Windows the
flow engine runs inside WSL2 until the native PTY host lands (th-2fbc9c).
