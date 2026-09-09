# SmoothFlow macOS — UI tests

The XCUITest lane for `apps/smoothflow` (pearl th-a58a97). Part of the
SmoothFlow test strategy in [SmoothFlow-Testing.md](SmoothFlow-Testing.md).

## What runs

| Suite               | Backend                                                                      | Asserts                                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SmoothFlowTests`   | none (pure)                                                                  | frame codec, store reducer, notifier, daemon address (48 XCTests)                                                                                                                                                                                                                                                                                                                                         |
| `MockServerUITests` | `mock/server.mjs` on a free port                                             | sidebar shows the fixture fleet with the right state labels (`approve`/`working`/`limit…`/`done`/`idle`); selecting a row hosts a surface with its header + path; ⌘I inbox shows the permission card and **Allow** flips the row to working; ⌘, settings renders the Permissions and Daemon panes                                                                                                         |
| `RealEngineUITests` | `flow_e2e_server` (the daemon's router + supervisor) + `fake-claude` on PATH | a session created over HTTP appears in the sidebar; steering `/work hello` reaches the fake agent, its hooks drive the row to `idle` and `/snapshot` shows `worked: hello`; `/perm` puts a hook-reported permission in the inbox and **Allow** is printed by the agent as the long-polled decision; ⌘⌥R (Kill & Resume) relaunches with `--resume` (`resume=1` on the pane, `--resume` in the path label) |

Every wait is a poll with a timeout (`waitForExistence`, `waitUntil`), never a
fixed sleep. A failing test attaches a screenshot to the result bundle.

## Launch contract (what the test passes the app)

| Env / arg                                         | Effect                                                                              |
| ------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `SMOOTHFLOW_DAEMON_ADDR=host:port`                | external mode — connect, never spawn a daemon or a tmux server                      |
| `SMOOTHFLOW_DAEMON_TOKEN`                         | the engine's local token (`X-Smooth-Token` / `?token=`)                             |
| `SMOOTHFLOW_UI_TEST=1`                            | Sparkle is not started (its first-run prompt would steal focus)                     |
| `HOME` + `CFFIXED_USER_HOME` → temp dir           | UserDefaults, `~/.smooth/*` reads and the onboarding flag never touch the real home |
| `-onboarded YES -ApplePersistenceIgnoreState YES` | no onboarding sheet, no restored windows                                            |

Accessibility identifiers the tests key on: `sidebar.session.<id>`,
`sidebar.title.<id>`, `sidebar.state.<id>`, `sidebar.connection`,
`sidebar.needsYou`, `pane.header`, `center.path`, `center.tabs`, `steer.field`,
`inbox.card.<id>`, `inbox.allow.<id>` / `inbox.deny.<id>` /
`inbox.allowSession.<id>`, `settings.pane.{permissions,attention,daemon}`.

## Run locally

```sh
brew install xcodegen tmux node
CARGO_TARGET_DIR=$HOME/.cargo/target-e2e cargo build -p smooai-smooth-daemon --example flow_e2e_server
cd apps/smoothflow && xcodegen generate
xcodebuild -project SmoothFlow.xcodeproj -scheme SmoothFlow -configuration Debug \
    -derivedDataPath build/DerivedData CODE_SIGN_IDENTITY=- test            # both suites
# one suite: -only-testing:SmoothFlowUITests/RealEngineUITests
```

`RealEngineUITests` looks for the server at
`~/.cargo/target-e2e/debug/examples/flow_e2e_server` (override with
`SMOOTHFLOW_E2E_SERVER`) and skips — with the reason — when it, `tmux` or
`node` is missing. Each test gets its own temp HOME, flow.db and tmux socket
(`flow-ui-<pid>`), torn down with the test; nothing touches `~/.smooth`.

Two local preconditions, both enforced by the harness rather than assumed:

- **Quit SmoothFlow.app first.** `XCUIApplication.launch()` kills any running
  instance of `ai.smoo.smoothflow` — i.e. your real app. The harness skips
  with that message instead of taking it down.
- **Accessibility trust for the runner.** `xctrunner` is spawned by
  `testmanagerd`, not your shell, so it does not inherit the terminal's
  Accessibility grant. Without it every element reads back with an empty
  label and the connection assertion fails at launch. Grant System Settings →
  Privacy & Security → Accessibility to Xcode (and Xcode Helper), or run the
  suite from Xcode once and accept the prompt. CI runners have it.

## CI

`.github/workflows/smoothflow-mac.yml` on every PR touching the app, the
example server or the fixture: caches GhosttyKit by `ghosttykit.lock`, builds
`flow_e2e_server` (rust-cache), runs `SmoothFlowTests` then
`SmoothFlowUITests` with `time` per suite, and uploads the `.xcresult` (with
failure screenshots) on failure. If the UI suite passes ~15 min on the runner,
split `MockServerUITests` (PR) from `RealEngineUITests` (nightly `schedule:`).
