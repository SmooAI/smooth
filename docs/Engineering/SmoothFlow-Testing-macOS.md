# SmoothFlow macOS — UI tests

The XCUITest lane for `apps/smoothflow` (pearl th-a58a97). Part of the
SmoothFlow test strategy in [SmoothFlow-Testing.md](SmoothFlow-Testing.md).

## What runs

| Suite               | Backend                                                                      | Asserts                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SmoothFlowTests`   | none (pure)                                                                  | frame codec, store reducer, notifier, daemon address (48 XCTests)                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `MockServerUITests` | `mock/server.mjs` on a free port                                             | sidebar shows the fixture fleet with the right state labels (`approve`/`working`/`limit…`/`done`/`idle`); selecting a row hosts a surface with its header + path; ⌘I inbox shows the permission card and **Allow** flips the row to working; **Close…** on a finished card → confirm sheet → `flow.close` removes the card and the sidebar row, and the unmerged fixture (`fs-3034dddd`) is refused on the card until **Force close**; ⌘, settings renders the Permissions and Daemon panes |
| `QuitUITests`       | `mock/server.mjs`, then spawn mode with a `trap '' TERM` "daemon"            | the Quit Apple event (`NSRunningApplication.terminate()`, what AppleScript `quit` and the release lane send) exits the app against the mock; in spawn mode with a child daemon that ignores SIGTERM the app still exits within the bounded shutdown and the daemon pid is gone (th-6198bf)                                                                                                                                                                                                  |
| `RealEngineUITests` | `flow_e2e_server` (the daemon's router + supervisor) + `fake-claude` on PATH | a session created over HTTP appears in the sidebar; steering `/work hello` reaches the fake agent, its hooks drive the row to `idle` and `/snapshot` shows `worked: hello`; `/perm` puts a hook-reported permission in the inbox and **Allow** is printed by the agent as the long-polled decision; ⌘⌥R (Kill & Resume) relaunches with `--resume` (`resume=1` on the pane, `--resume` in the path label)                                                                                   |

Every wait is a poll with a timeout (`waitForExistence`, `waitUntil`), never a
fixed sleep. A failing test attaches a screenshot to the result bundle.

## Launch contract (what the test passes the app)

| Env / arg                                          | Effect                                                                                                                                                          |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SMOOTHFLOW_DAEMON_ADDR=host:port`                 | external mode — connect, never spawn a daemon or a tmux server                                                                                                  |
| `SMOOTHFLOW_DAEMON_TOKEN`                          | the engine's local token (`X-Smooth-Token` / `?token=`)                                                                                                         |
| `SMOOTHFLOW_DAEMON_BIN` + `TMUX_TMPDIR` → temp dir | spawn mode (`QuitUITests`): the app supervises this binary; its `tmux -L smoothflow` server lives under the temp dir, so kill-server never reaches a real fleet |
| `SMOOTHFLOW_UI_TEST=1`                             | Sparkle is not started (its first-run prompt would steal focus)                                                                                                 |
| `HOME` + `CFFIXED_USER_HOME` → temp dir            | UserDefaults, `~/.smooth/*` reads and the onboarding flag never touch the real home                                                                             |
| `-onboarded YES -ApplePersistenceIgnoreState YES`  | no onboarding sheet, no restored windows                                                                                                                        |

Accessibility identifiers the tests key on: `sidebar.session.<id>`,
`sidebar.title.<id>`, `sidebar.state.<id>`, `sidebar.connection`,
`sidebar.needsYou`, `pane.header`, `center.path`, `center.tabs`, `steer.field`,
`inbox.card.<id>`, `inbox.allow.<id>` / `inbox.deny.<id>` /
`inbox.allowSession.<id>`, `inbox.finished.<id>`, `inbox.close.<id>` (opens the
sheet: `inbox.close.pearl` / `inbox.close.worktree` toggles, `inbox.close.confirm`
/ `inbox.close.cancel`), `inbox.close.refusal.<id>` / `inbox.close.force.<id>`,
`settings.pane.{permissions,attention,daemon}`.

## Run locally

```sh
brew install xcodegen tmux node
CARGO_TARGET_DIR=$HOME/.cargo/target-e2e cargo build -p smooai-smooth-daemon --example flow_e2e_server
cd apps/smoothflow && xcodegen generate
xcodebuild -project SmoothFlow.xcodeproj -scheme SmoothFlow -configuration Debug \
    -derivedDataPath build/DerivedData CODE_SIGN_IDENTITY=- test            # both suites
# one suite: -only-testing:SmoothFlowUITests/RealEngineUITests
```

`RealEngineUITests` looks for the server at `<repo>/target/debug/examples/`
then `~/.cargo/target-e2e/debug/examples/` (`SMOOTHFLOW_E2E_SERVER` overrides,
but `xctrunner` inherits neither the shell's env nor `TEST_RUNNER_` forwarding
on macOS — build into one of those two paths) and skips — with the reason — when it, `tmux` or
`node` is missing. Each test gets its own temp HOME, flow.db and tmux socket
(`flow-ui-<pid>`), torn down with the test; nothing touches `~/.smooth`.

**Quit SmoothFlow.app first.** `XCUIApplication.launch()` kills any running
instance of `ai.smoo.smoothflow` — i.e. your real app. The harness skips with
that message instead of taking it down.

Reading text: on macOS an `AXStaticText` carries its string in `value`, not
`label` (`label` is the AXDescription, usually empty) — `FlowUITestCase.label(_:)`
reads `value` first. Match elements by identifier, never by copy.

## CI

`.github/workflows/smoothflow-mac.yml` on every PR touching the app, the
example server or the fixture: caches GhosttyKit by `ghosttykit.lock`, builds
`flow_e2e_server` (rust-cache), runs `SmoothFlowTests` then
`SmoothFlowUITests` with `time` per suite, and uploads the `.xcresult` (with
failure screenshots) on failure. If the UI suite passes ~15 min on the runner,
split `MockServerUITests` (PR) from `RealEngineUITests` (nightly `schedule:`).
