# SmoothFlow Client Spec

> Epic th-3e6020. The one description every SmoothFlow client is built from,
> and the contract that keeps them identical.
> Engine and wire protocol: [SmoothFlow.md](SmoothFlow.md). Keyboard design
> notes: [SmoothFlow-Keybindings.md](../Engineering/SmoothFlow-Keybindings.md).

## 1. Clients and the parity rule

| Client                     | Platform       | Stack                                                    | Terminal                                                                 |
| -------------------------- | -------------- | -------------------------------------------------------- | ------------------------------------------------------------------------ |
| **SmoothFlow for Mac**     | macOS          | Swift/AppKit (`apps/smoothflow`)                         | libghostty, Metal                                                        |
| **SmoothFlow Desktop**     | Linux, Windows | Rust + GPUI (`apps/smoothflow-desktop`, planned)         | libghostty-vt state, GPUI GPU renderer (Vulkan / DirectX)                |
| **SmoothFlow for iOS**     | iPhone, iPad   | SwiftUI (`smooai/apps/smoothflow-mobile/ios`)            | libghostty, Metal (planned; text snapshot today)                         |
| **SmoothFlow for Android** | Android        | Kotlin/Compose (`smooai/apps/smoothflow-mobile/android`) | libghostty-vt + hardware-accelerated grid (planned; text snapshot today) |

**Every client renders its terminals on the GPU.** Web and Electron are not
SmoothFlow clients.

**Parity rule.** A user-visible behavior is described here first, then shipped
to all four clients in the same epic. The mock flow server gains a fixture for
it. An epic is not done while any client is missing it. When a platform
genuinely can't do something the same way (for example, no hardware keyboard
on a phone), this spec names the equivalent. The clients never invent their own.

Normative words: **must**, **should**, **may**.

## 2. Connection

- **Desktop clients** talk to the daemon directly:
    - `GET/POST /api/flow/*` and the WS at `/api/flow/ws`.
    - The local token goes in `X-Smooth-Token` on HTTP and `?token=` on the WS.
    - Token resolution: `$SMOOTHFLOW_DAEMON_TOKEN` → `$SMOOTH_LOCAL_TOKEN` → `~/.smooth/operator-token`.
    - Daemon lookup: `$SMOOTH_FLOW_ADDR` → `~/.smooth/flow.addr` → `~/.smooth/daemon.addr`.
- **The macOS app** also launches and supervises its own daemon. The Linux app
  should do the same. On Windows the app starts the daemon **inside WSL2**
  (`wsl.exe -d <distro> -- smooth-daemon operator --addr 127.0.0.1:<port>`) and
  reaches it over localhost, until the native PTY host lands (th-2fbc9c).
- **Phones** reach a paired daemon through Smoo Relay with end-to-end encrypted
  flow frames ([SmoothFlow.md § End-to-end encryption](SmoothFlow.md#end-to-end-encryption)).
  Only WS frames cross the relay, so a phone feature can never depend on an
  HTTP-only route. Anything a phone needs must exist as a frame too.
- A client must show the connection state: connecting, connected (with the
  machine label), or offline (with the reason). A phone must also say which
  daemon it's paired to.

## 3. Model

- **Session** (the `flow.session` payload):
    - `id`, `kind` (a harness name or `shell`), `title`, `project`, `worktree`, `branch`, `pearl_id`
    - `state`: one of `starting | working | idle | needs_you | limited | done | dead`
    - `attention`: `{reason, detail, request_id, resume_at}`
    - `unread`, `exit_code`, `state_source` (`hooks | native | inferred`)
    - `done` and `dead` are terminal.
- **Attention reasons:** `permission` and `question` (approvable only when they
  carry a `request_id`), `usage_limit` (with `resume_at`), `crashed`, `held`, `unknown`.
- **Harness row** (`flow.hello.harnesses` / `flow.harnesses`): `name`,
  `display_name`, `installed`, `reason`, `state_source`, `hidden`,
  `order_index`, `origin`, and `health: {verdict: works|degraded|not_installed, reason?, fix?}`.
- **Repo row** (`GET /api/flow/repos`): `path`, `name`, `branch?`, `main?` (for a linked worktree), `touched`.
- **Inferred context** (`GET /api/flow/infer?cwd=`): `worktree`, `project`,
  `is_git`, `branch`, `pearl_id`, `pearl_title`, `jira_key`, `title`.

## 4. Layout

```
┌ Fleet sidebar ┐┌ Center ─────────────────────────────────┐┌ Pearl rail ┐
│ group: project││ terminal | diff | PR | activity  · argv  ││ pearl      │
│  ● session    ││ [tab strip — only with ≥ 2 tabs]          ││ handoff    │
│  ● session    ││ pane tree (splits)                        ││ Checkpoint │
│ group: shells ││                                           ││ Hand off   │
│ status line   ││ steer bar                                 ││ Close      │
└───────────────┘└──────────────────────────────────────────┘└────────────┘
```

- **Fleet sidebar.**
    - Sessions are grouped by project, in first-seen order, with plain shells last under "shells".
    - Each row shows a state dot, title, state word and unread badge. Needs-you rows are tinted amber.
    - The footer shows the connection and the counts: working, need you, done, idle.
    - Toggle: `toggleSidebar`.
- **Center tabs** (`CenterTabGate`):
    - **terminal** is always available.
    - **diff** is available when the session sits in a git repo.
    - **PR** needs a repo _and_ a branch, and shows an empty state when there's no PR.
    - **activity** depends on the harness: full events for `hooks`/`native`, supervision events only for `scrape`, nothing for a shell.
    - Tabs are **disabled, never hidden**.
- **Steer bar:** `⌘↩` sends to the focused session; `steerAll` sends to every working one.
- **Pearl rail:** the pearl, the handoff packet (worktree, branch, HEAD, dirty files, session), and Checkpoint, Hand off and Close.
- **Phones** show the same content as a stack: fleet list → session → a segmented terminal / diff / PR / activity control, with the pearl rail as a sheet.

## 5. Surfaces: tabs and splits

- **Tabs hold panes, not sessions.** A pane shows one session, and several
  panes or tabs may show the same one. Closing a view never ends a session.
- **New tab** opens on the focused session. **New Shell Here** opens a tab
  holding a new shell in the focused session's worktree.
- **Tab titles** come from `Session.tabTitle`: the pearl id, else the title,
  else the worktree's folder name (`~` for home). A path-shaped title is
  shortened to its last component, so a tab is never titled with a whole path
  or a bare `/`. The tab strip shows only when there are two or more tabs.
- **Splits.** Right, down, left and up split the focused pane. The new pane
  shows the session it was split from. There are directional focus moves (only a pane lying beyond the focused pane's edge counts as "that way"; the nearest edge wins, then the nearest centerline, then the lower pane id),
  zoom (the focused pane fills the tab, and again restores it) and equalize.
- **Close semantics** (`PaneClose`, the scope order is normative):
    1. `closePane` closes the focused pane (scope `pane`).
    2. When it's the tab's last pane, it closes the tab (scope `tab`).
    3. When it's the last pane of the last tab, it **empties** the pane (scope `last`). The **window is never closed** by this action.
- **When closing asks first.** It asks only when the pane shows a live session
  with something running (an idle shell at a prompt doesn't count) that no
  other pane or tab also shows, and only while the setting is on. The dialog
  offers **Close <Pane|Tab>**, which leaves the session running in the fleet,
  or **End Session**. **Cancel is the default button.** "Don't ask again"
  turns the setting off.
- **Phones.** There are no splits. A session view is one full-screen surface,
  and back returns to the fleet. Closing a view there never ends a session;
  ending one is an explicit action.

## 6. New Session

- **Kind picker.** The engine's harness list, in the user's order, with hidden
  harnesses omitted and Shell last.
    - A **not installed** harness is disabled, with its reason and the install command to copy.
    - A **degraded** harness (health `degraded`) is labelled "needs setup", shows the reason and the fix, and **stays startable**.
- **Directory.** A first-class field, starting on the inferred worktree.
    - Typing searches the repo index. ↑/↓ move, Return picks and Esc clears (phones: a search list).
    - A typed path (`~/…`, `/…`) is used as-is.
    - **Browse…** opens the platform folder picker.
    - Picking re-runs inference, so the pearl, branch and title follow.
- **Inferred context box:** the title, then pearl · Jira · branch · worktree, with "not a git worktree" or "no pearl here — starting anyway is fine".
- **Prompt:** optional.
- **Override context** (collapsed): pearl id and title.
- **Start** is never blocked on a missing pearl.
- **Default directory.** When nothing else applies, the daemon's workspace is
  `$HOME`, never `/`. An app launched from Finder or the Start menu must not
  inherit `/`.

## 7. Acting on sessions

- **Approve / Deny.** Only a session whose attention carries a `request_id` is
  approvable. Clients must show the command or question being asked. Phones
  show the approval card inline in the fleet list.
- **Kill** stops the session, and **Kill & Resume** relaunches it. Both ask
  first, naming the harness and the state, because an agent killed mid-turn
  loses its in-flight work.
- **Close Out** (`closeOut`) ends the session for good: it closes the pearl,
  removes the merged worktree and branch, and drops the row. It works from the
  sidebar menu, the Session menu or the card, and a live session is killed
  first.
- **Fan out** runs one prompt against N candidates, each with its own
  worktree and child pearl. **Pick winner** merges one and garbage-collects
  the others.

## 8. Attention and notifications

- A session entering `needs_you`, `limited` or `crashed`, or finishing a turn
  while not focused, marks it **unread**. Focusing a session clears unread.
- **Notifications:** native OS notifications on desktop and push on phones,
  per the notification settings. They are suppressed for the focused session
  while the app is frontmost. Clicking one focuses that session.
- The app badge (dock or launcher) shows the needs-you count.
- _Planned (th-b1bae8):_ a "done but unseen" state rolled up pane → tab →
  project, jump to next unread, and a desktop approvals feed.

## 9. Keymap

The Mac app's `FlowAction` enum is the source of truth, and
`~/.smooth/smoothflow/keybindings.toml` overrides it by name. On Linux and
Windows the primary modifier is **Ctrl+Shift**, because bare Ctrl+letter
belongs to the program in the terminal, as in GNOME Terminal, Konsole and
Windows Terminal. The fleet-action family (`⌘⌥` on the Mac) is **Ctrl+Alt**.

| Action                            | macOS                | Linux / Windows                                               |
| --------------------------------- | -------------------- | ------------------------------------------------------------- |
| New Session                       | ⌘N                   | Ctrl+Shift+N                                                  |
| New Shell Here                    | ⌘⇧T                  | Ctrl+Shift+Alt+T                                              |
| Fan Out                           | ⌘⇧N                  | Ctrl+Shift+Alt+N                                              |
| Steer Focused / Steer All Working | ⌘↩ / ⌘⌥↩             | Ctrl+Enter / Ctrl+Alt+Enter                                   |
| Allow / Deny                      | ⌘⌥Y / ⌘⌥N            | Ctrl+Alt+Y / Ctrl+Alt+N                                       |
| Kill & Resume / Kill              | ⌘⌥R / ⌘⌥K            | Ctrl+Alt+R / Ctrl+Alt+K                                       |
| Close Out                         | ⌘⌥W                  | Ctrl+Alt+W                                                    |
| Focus Session 1–9                 | ⌘1–⌘9                | Alt+1–Alt+9                                                   |
| New Tab / Close Pane / Close Tab  | ⌘T / ⌘W / ⌘⇧W        | Ctrl+Shift+T / Ctrl+Shift+W / Ctrl+Shift+Alt+W                |
| Previous / Next Tab               | ⌘⇧[ / ⌘⇧]            | Ctrl+PageUp / Ctrl+PageDown                                   |
| Split Right / Down / Left / Up    | ⌘D / ⌘⇧D / ⌘⇧← / ⌘⇧↑ | Ctrl+Shift+D / Ctrl+Shift+Alt+D / Ctrl+Shift+← / Ctrl+Shift+↑ |
| Focus Pane ←→↑↓                   | ⌘⌥ arrows            | Ctrl+Alt arrows                                               |
| Zoom Pane / Equalize              | ⌘⇧↩ / ⌘⌥=            | Ctrl+Shift+Enter / Ctrl+Alt+=                                 |
| Inbox                             | ⌘I                   | Ctrl+Shift+I                                                  |
| Terminal / Diff / PR / Activity   | ⌘⌥1–4                | Ctrl+Alt+1–4                                                  |
| Toggle Sidebar / Pearl Rail       | ⌃⌘S / ⌃⌘P            | Ctrl+Shift+Alt+S / Ctrl+Shift+Alt+P                           |
| Settings                          | ⌘,                   | Ctrl+,                                                        |

Every action is also reachable from the menus (desktop) or an explicit control
(phones). Phones have no shortcut layer, so every action must have a visible
control.

## 10. Terminal requirements (all clients)

- **GPU-rendered**, at the display's refresh rate, with no visible tearing when
  scrolling a full screen of Claude Code output.
- **UTF-8 always.** The app must hand its PTY children a UTF-8 locale; a
  launchd/Finder launch has no `LANG`, which turned every Nerd Font glyph into
  `_`. Bundle a Nerd Font, and the default theme is Catppuccin Mocha, from
  Ghostty theme files.
- Attach sends `flow.attach{cols,rows}`. Output is `flow.output{seq,data_b64}`,
  bytes written to the terminal in `seq` order. Input is `flow.input{data_b64}`
  (keyboard and mouse encodings, bracketed paste). Resize sends `flow.resize`.
  A late joiner gets a full redraw from the engine.
- IME, selection and copy, links, scrollback search, and font size zoom.
- Phones must render live `flow.output` in a real terminal. `flow.screen` text
  is for thumbnails and the fleet list only.

## 11. Settings

- **Terminal:** font, size, theme, and confirm-before-closing a live pane.
- **Harnesses:** order and hide, with health and fix.
- **Keyboard:** rebind any action, with conflicts shown and never silently dropped.
- **Notifications:** per reason, and sound.
- **Daemon:** the app's own daemon, or connect to an address.
- **Phones:** pairing.
- **Smoo:** connection and usage (th-8b3918).

## 12. Conformance

- **Vectors.** Pure client logic lives once in `crates/smooth-flow-client`,
  which writes JSON test vectors to `spec/vectors/*.json`. Every client
  replays them in its own unit tests: XCTest on Mac and iOS, cargo test in the
  desktop app, JUnit on Android. Divergence fails CI.
    - The pure logic covered: pane tree ops, the close scope and decision, tab
      titles, the center tab gate, directory picking (`abbreviate`,
      `expandedPath`, `moved`), keymap parsing and conflicts, the harness
      picker's label, default and degraded, and fleet grouping and counts.
- **Scenarios.** A shared list of UI scenarios, each run by every client's UI
  test harness against the mock flow server:
    - new session in a picked directory
    - type into the terminal
    - split, then close pane → tab → last (the window stays)
    - approve and deny
    - close out
    - degraded harness badge
    - the phone's pairing path
- **Mock server:** `apps/smoothflow/mock` and `smoothflow-mobile/mock` gain a
  fixture for every new frame or field.
- **UI tests run in CI only.** They never run on a developer's Mac, where
  macOS UI automation asks for a password.

## 13. Change log

- 2026-09-26: first version, written from SmoothFlow for Mac 0.2.8 and the
  0.2.x phone apps (th-3e6020). It records the ⌘W "last pane empties" rule
  (th-96fcb7), the Directory field (th-145e6b), degraded harnesses
  (th-51bf88), the `$HOME` default workspace, and the Linux/Windows keymap.
