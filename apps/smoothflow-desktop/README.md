# SmoothFlow Desktop (Linux + Windows)

The SmoothFlow agent fleet console for Linux and Windows, with GPU-rendered
terminals, built on [GPUI](https://www.gpui.rs/) through
[`gpui-kit`](https://crates.io/crates/gpui-kit). Epic th-3e6020.

It implements the [SmoothFlow Client Spec](../../docs/Architecture/SmoothFlow-Client-Spec.md),
the same spec the Mac app (`apps/smoothflow`) and the phones follow. Its rules
come from `crates/smooth-flow-client`, whose conformance vectors every client
replays.

## Status (M2)

- Connects to the flow engine the way every client does: `$SMOOTH_FLOW_ADDR` →
  `~/.smooth/flow.addr` → `~/.smooth/daemon.addr`, with the local token.
  It reconnects with backoff.
- **Fleet sidebar**: grouped by project with shells last, a state dot, titles
  from the shared rules, an unread dot, needs-you rows tinted amber, a
  **+ New** button, and the connection and counts footer.
- **Tabs and splits** (spec §5): a tab strip (shown with two or more tabs) and
  a pane tree per tab. Split right/down/left/up, directional focus, zoom,
  equalize, New Tab, New Shell Here, previous/next tab, and Close Pane /
  Close Tab with the pane → tab → last scope (the last pane empties; the
  window never closes). Closing asks first only when a live, running session
  would lose its last view, offering Close or End Session with Cancel as the
  default. Clicking a fleet row shows that session in the focused pane.
- **Terminals**: each pane is a live PTY stream (`flow.attach` /
  `flow.output`) parsed by `alacritty_terminal` and drawn by GPUI (Vulkan on
  Linux, DirectX on Windows, Metal on macOS). Cell metrics are measured from
  the font. The cursor is a block in the focused pane and hollow elsewhere.
  Each pane resizes with the layout.
- **One session in several panes**: a session has one PTY, so one size. It is
  attached at the **smallest** size of the panes showing it, per axis (tmux's
  `window-size smallest`). Every pane shows the whole screen, and larger panes
  have spare space at the right and bottom. Using the focused pane's size would
  clip the other panes and make the program redraw on every focus move.
- **New Session** (spec §6): a kind picker from `flow.hello` /
  `flow.harnesses` (hidden omitted, Shell last, not-installed disabled with its
  reason, degraded marked "needs setup" and still startable, with the fix to
  copy). The Directory field searches the repo index (`GET /api/flow/repos`)
  or takes a typed `~/…` or `/…` path, and picking re-runs inference
  (`GET /api/flow/infer`). There is an inferred-context box and an optional
  prompt. Start sends `flow.new`, and the new session opens in the focused pane
  (or a new tab when that pane is busy).
- **Approvals** (spec §7): when the focused session's attention is an
  approvable `permission` or `question` (it has a `request_id`), a bar shows
  the command or question with Allow and Deny (`flow.approve`).
- **Keymap** (spec §9): the Linux/Windows table (Ctrl+Shift primary, Ctrl+Alt
  for fleet actions, Alt+1–9 to focus a session), overridable by name in
  `~/.smooth/smoothflow/keybindings.toml` (the same file and names as the Mac).
  Problems and conflicts are printed at startup.

The rules (pane trees, tabs and close scope, titles, picker rows,
approvability, keymap) come from `crates/smooth-flow-client`. This app's own
pure logic (layout math, the sheet, the text field, frames and HTTP) is
unit-tested. Everything the window does (engine events, New Session,
attach/resize, keystrokes, close/kill) lives in the toolkit-free
`app_core::Core` in the crate's library. The GPUI `Workspace` wraps it and
the views only draw.

Not yet: Close Out, Fan Out, steer bar, Inbox, diff/PR/activity tabs, the
pearl rail, Settings (the "Don't ask again" choice lasts until you quit),
menus, selection and copy, scrollback, IME in the sheet's fields, Browse… (the
platform folder picker), a bundled Nerd Font, and libghostty-vt as the VT
backend.

## Build

```bash
cd apps/smoothflow-desktop
cargo run            # needs a running flow engine (SmoothFlow's daemon or `th up`)
cargo test           # unit tests + the e2e below (needs tmux; builds smooth-daemon)
```

It's its own Cargo workspace, so GPUI never enters the main `th` build. On
Linux it needs the X11/Wayland/Vulkan/fontconfig/D-Bus/PipeWire development
packages; `.github/workflows/smoothflow-desktop.yml` lists them. On Windows the
flow engine runs inside WSL2 until the native PTY host lands (th-2fbc9c).

## End-to-end test

`tests/e2e.rs` runs the app's core against a **real `smooth-daemon`**
(th-032792). It never opens a window. It boots an isolated daemon with a
scratch `$HOME`, an ephemeral port, its own `tmux -L` socket, no relay and no
`tailscale serve`, so it never touches a daemon you have running. It then
discovers the daemon from that HOME's `flow.addr` and `operator-token`,
connects, gets `flow.hello`, and opens New Session (whose repo and inference
reads go to the daemon). It picks **Shell** with the arrow keys, presses
Enter, waits for the session to open in the focused pane and attach at 80x24,
and waits for live output. Then it types `echo smoothflow-e2e-$((N+1))`
through the key encoder and checks that the `TerminalModel` shows the
computed line. Finally it kills the session through the Kill confirmation.

```bash
cd apps/smoothflow-desktop
# Use a daemon you already built…
SMOOTHFLOW_E2E_DAEMON=/path/to/target/debug/smooth-daemon cargo test --test e2e
# …or let the test build it from the main workspace (cargo build -p
# smooai-smooth-daemon into $CARGO_TARGET_DIR, or <repo>/target when unset).
cargo test --test e2e
```

It needs tmux. Without tmux the test skips, unless `SMOOTH_E2E_STRICT=1`
(which CI sets), which turns the skip into a failure. The test is Unix only:
Windows has no native tmux. CI runs it on Linux and macOS in
`.github/workflows/smoothflow-desktop.yml`, which builds the daemon first and
passes it in through `SMOOTHFLOW_E2E_DAEMON`.

`a_shell_session_gets_a_live_terminal_from_a_finder_launched_daemon` runs the
daemon with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, the PATH Big Smooth.app and
Finder start it with. That PATH caused "I created sessions and never got a
terminal" (th-9f6814, fixed in #707).

`a_session_that_cannot_launch_shows_why` points the daemon at a tmux that
doesn't exist. It checks that the session reaches the app as `dead` with
attention `launch_failed` and a detail naming the problem, that nothing is
left in `starting`, and that the pane shows `shell is dead: launch failed: …`
(`Core::pane_hint`) instead of a blank terminal.
