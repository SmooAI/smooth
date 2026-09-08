# SmoothFlow — the macOS shell

Pearl th-f7f823 (lane B of the SmoothFlow epic th-6ac036). Source: `apps/smoothflow/`.

SmoothFlow is the native macOS fleet console for Smooth agents: a sidebar of
every session grouped by project, a real terminal per session (libghostty), an
inbox of what needs a human, and a pearl rail. **The app holds no state.** Every
fact on screen arrived as a `flow.*` frame from the engine (`smooth-flow`, lane
A, hosted in `smooth-daemon`); every decision leaves as a `flow.*` frame. The
protocol is [`smoothflow-protocol.md`](../../apps/smoothflow/mock/protocol.md)
(v0 contract, mocked by `apps/smoothflow/mock/server.mjs`).

![main](assets/smoothflow/main.png)

## Shape

```
apps/smoothflow/
├── project.yml                  xcodegen spec (the .xcodeproj is generated, gitignored)
├── Info.plist / entitlements.plist   hand-maintained — see "xcodegen traps"
├── LaunchAgents/…daemon.plist   SMAppService agent, copied to Contents/Library/LaunchAgents
├── Sources/
│   ├── Flow/        FlowFrame (codec) · FlowStore (reducer) · FlowClient (WS) · DaemonAddress · DaemonManager
│   ├── Terminal/    GhosttyRuntime (one ghostty_app) · TerminalSurfaceView (NSView per session)
│   ├── UI/          MainWindow (AppKit split) · CenterViewController (tabs, splits, steer bar)
│   │                FleetSidebar · PearlRail · InboxView · Sheets · SettingsView (SwiftUI in NSHostingView)
│   ├── Permissions/ Permissions (TCC status + asks) · AttentionNotifier (attention → UNNotification)
│   └── App/         AppController (coordinator) · AppDelegate (menus, shortcuts)
├── Tests/           XCTest: frame codec, store reducer, notification mapping, address resolution
├── mock/server.mjs  zero-dependency mock flow engine (node)
└── scripts/         ensure-ghosttykit.sh · ghosttykit.lock · build-release.sh · tcc-probe.sh
```

AppKit owns the window, splits, terminal, tab strip, steer bar and menus;
the list/card/form surfaces (sidebar, rail, inbox, sheets, settings) are SwiftUI
hosted in `NSHostingView` — same product, a third of the lines.

## Terminal: libghostty in MANUAL I/O mode

The engine owns every PTY (agents run under tmux on the engine side). The
shell therefore uses the manaflow-ai/ghostty fork's **manual I/O surface
mode** (`GHOSTTY_SURFACE_IO_MANUAL`): ghostty spawns nothing, `flow.output`
bytes are pushed in with `ghostty_surface_process_output`, and whatever the
user types comes back out of the `io_write_cb` already terminal-encoded and is
sent as `flow.input`. Upstream ghostty has no such API — this is why the pinned
`GhosttyKit.xcframework` is the fork's prebuilt (see `scripts/ghosttykit.lock`;
`ensure-ghosttykit.sh` verifies the sha256 and refuses anything else).

One `TerminalSurfaceView` per session, created on first focus and kept for the
session's life, so scrollback lives in the surface. Keys go through
`interpretKeyEvents` (IME/dead keys work) then `ghostty_surface_key`; mouse,
scroll (precise + momentum bits), resize (`flow.resize` on grid change), focus
and content scale are forwarded. `⌘D` splits the surface area (NSSplitView,
no third-party splitter — bonsplit's submodule is not vendored here).

## Daemon lifecycle and TCC — the part cmux gets wrong

macOS attributes a TCC decision to the **responsible process**, and only an app
bundle's main executable launched through LaunchServices can _ask_. A daemon
started from a terminal inherits the terminal's grants and can never prompt, so
"grant Calendar to Smooth" silently fails. SmoothFlow therefore never adopts a
daemon it did not start:

- **Child mode (default):** the app spawns `smooth-daemon operator --addr 127.0.0.1:<free port>`
  (binary: bundled `Contents/MacOS/smooth-daemon` → `~/.cargo/bin` → `PATH`),
  restarts it with backoff if it dies, and kills it on quit. The daemon writes
  `~/.smooth/daemon.addr` as usual so `th flow` finds it.
- **LaunchAgent mode (Settings ▸ Daemon):** `SMAppService.agent` registers
  `Contents/Library/LaunchAgents/ai.smoo.smoothflow.daemon.plist` (fixed port
  8791). Only offered when the daemon is bundled, i.e. release builds.
- **tmux:** the app starts the `smoothflow` tmux server itself
  (`tmux -L smoothflow`) and kills it on quit — see the matrix for why.
- `SMOOTHFLOW_DAEMON_ADDR` / Settings ▸ Daemon override the address for the
  mock server. `~/.smooth/daemon.addr` is deliberately **not** consulted: it
  advertises whichever daemon started last, terminal ones included.

### TCC matrix (measured 2026-09-07, macOS 26.4, Developer-ID-signed ad-hoc-equivalent build)

Probe: `scripts/tcc-probe.sh` — prints the responsible pid
(`responsibility_get_pid_responsible_for_pid`, `scripts/responsible.c`; root-free
unlike `launchctl procinfo`), an FDA read (`open()` on TCC.db — `stat()` is not
gated), and `smooth-daemon tcc calendar`.

| Permission            | Who asks                                                           | Who inherits                                     | How verified                                                                                                                                    |
| --------------------- | ------------------------------------------------------------------ | ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Notifications         | app (`UNUserNotificationCenter.requestAuthorization`)              | n/a — only the app posts                         | `Permissions.refresh()` reads `getNotificationSettings`                                                                                         |
| Calendar              | app main executable (`EKEventStore.requestFullAccessToEvents`)     | daemon, tmux server, every pane, grandchild tmux | prompt appears (screenshot below); pane in app's tmux: `calendar: granted`, `ical calendars` lists calendars; terminal shell: `not-determined`  |
| Reminders             | app (`requestFullAccessToReminders`)                               | same as Calendar                                 | same code path; `smooth-daemon tcc reminders`                                                                                                   |
| Full Disk Access      | nobody can prompt — Settings pane deep link + re-probe on activate | daemon, tmux, panes                              | pane in app's tmux: `fda=denied` while the same probe from the terminal (cmux has FDA) is `granted` — attribution follows the app, not the user |
| Automation (Messages) | first Apple Event from the app (`NSAppleEventsUsageDescription`)   | children                                         | `AEDeterminePermissionToAutomateTarget(askUserIfNeeded: false)` reads status without prompting                                                  |

**Attribution findings (the question the lane was asked):**

| Process                                                                    | Responsible pid                   | Notes                                                                                                                                      |
| -------------------------------------------------------------------------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| SmoothFlow launched via `open`                                             | itself                            | launched from a shell instead: responsible = the terminal, FDA/Automation read as _granted_ — a false reading. Always test through `open`. |
| `smooth-daemon` child of the app                                           | the app                           |                                                                                                                                            |
| `tmux -L smoothflow` server started by the app (daemonizes, ppid 1)        | **the app**                       | daemonizing does not break attribution                                                                                                     |
| pane inside that server                                                    | the app                           | `calendar: granted` after the app's grant                                                                                                  |
| tmux server started _from inside_ that pane (models daemon → tmux → agent) | **the app**                       | any depth inherits while the app lives                                                                                                     |
| the same servers **after the app quits**                                   | **themselves** (`bash`, `tmux`)   | attribution is lost the moment the responsible app exits; what an orphan then gets depends on its own binary's TCC rows — unpredictable    |
| relaunched app adopting the surviving server                               | still the old server → **itself** | adoption does not re-attribute                                                                                                             |

So: **a daemon restart is fine (the app is still the responsible process), an
app quit is not.** SmoothFlow kills its `smoothflow` tmux server on quit and
recreates it on launch (`DaemonManager.startTmuxServer` does `kill-server` first),
and the engine resumes agents with `--resume` — quitting the console is a
fleet restart by design, never a fleet with unattributed processes. Lane A's
engine must launch agents under `tmux -L smoothflow` (the app-owned server)
for this to hold — filed as a follow-up pearl.

![settings](assets/smoothflow/settings.png)

### Two build traps that silently kill every prompt

Both were hit while measuring the matrix, both produce `granted=false`, no
error, status `notDetermined`, nothing in tccd's log:

1. **xcodegen's `info:` and `entitlements:` keys regenerate those files** as
   near-empty plists. `project.yml` therefore points at them with plain
   `INFOPLIST_FILE` / `CODE_SIGN_ENTITLEMENTS` build settings and the files
   are hand-maintained. `build-release.sh` greps both back out of the signed
   app and fails the build if either is missing.
2. **Hardened runtime without `com.apple.security.personal-information.calendars`**
   (same for `.reminders`) — known from Big Smooth (th-36da65), still true.

## Attention → notifications

`AttentionNotifier.notification(for:settings:)` is a pure map from a session
to a notification (or nil). Every reason — permission, question, usage limit
(with resume time), crashed, held, finished — has its own toggle in
Settings ▸ Attention (`NotifySettings`, UserDefaults). The notification's
`userInfo` carries the session id and request id; clicking it focuses the
session, and permission notifications carry Allow / Deny actions that send
`flow.approve` straight from Notification Center. One live notification per
session (identifier `session:<id>`), cleared when the session is focused.

![inbox](assets/smoothflow/inbox.png)

## Keyboard

| Keys      | Action                                     |
| --------- | ------------------------------------------ |
| ⌘N / ⌘⇧N  | new session / fan out                      |
| ⌘I        | inbox                                      |
| ⌘↩ / ⌘⇧↩  | steer focused / steer all working          |
| ⌘1…9      | focus session N                            |
| ⌘⌥Y / ⌘⌥N | allow / deny the focused session's request |
| ⌘⌥R / ⌘⌥K | kill & resume / kill                       |
| ⌘⌥1/2/3   | terminal / diff / PR tab                   |
| ⌘D / ⌘⇧W  | split / close split                        |

![fan-out](assets/smoothflow/fanout.png)

## Signing and release

`scripts/build-release.sh`: Release build → bundle `smooth-daemon` → codesign
(nested first, then the bundle with `entitlements.plist`, hardened runtime) →
DMG → `scripts/macos/notarize-and-staple.sh`. `.github/workflows/smoothflow-mac.yml`
builds and tests ad-hoc on PRs touching `apps/smoothflow/**`, and signs +
notarizes on `workflow_dispatch` with the secrets `desktop-publish.yml` already
uses. The Developer ID identity string differs from desktop-publish's bare org
name; the workflow normalizes it.

## Gaps (pearls filed from the main checkout)

- Engine must run agents under the app-owned `tmux -L smoothflow` server so
  attribution holds across daemon restarts.
- `SMOOTH_ALLOW_SECOND_DAEMON=1` is set for the child: the daemon still shares
  `operator-storage.db` with a running Big Smooth; needs its own store.
- Diff tab shells `git diff` in the worktree; the PR tab only shows what the
  handoff endpoint reports (`pr.number/url/ci`). "Merge" opens the PR.
- "Close pearl + GC worktree" from the inbox is not in the v0 protocol.
- The Developer ID / notarized release path is wired but only the ad-hoc and
  locally re-signed builds were exercised.
