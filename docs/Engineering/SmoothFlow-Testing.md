# SmoothFlow — testing strategy

Pearl th-8e3087 (epic th-6ac036). Brent: "can we write similar tests to
orca / cmux". This page is the map: what each layer proves, where it runs,
how long it takes, and how to add to it. The engine itself is
[Architecture/SmoothFlow.md](../Architecture/SmoothFlow.md); the macOS UI
lane has its own page, [SmoothFlow-Testing-macOS.md](SmoothFlow-Testing-macOS.md).

## What cmux and orca do, and what we took

| Project  | Their e2e                                                                                                          | Ours                                                                                                                                                                                                                                                        |
| -------- | ------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **orca** | a "golden stub agent" — a scripted binary on PATH that behaves like the real agent where the orchestrator can tell | `tests/fixtures/fake-agent`: launched through a **harness manifest** like any coding CLI; honours `--session-id` / `--resume`, posts hook events, long-polls a permission, prints the usage-limit banner, exits with a code — in four state-source flavours |
| **orca** | the orchestrator booted for real, driven over its API, state asserted by polling                                   | a REAL `smooth-daemon` per test (isolated HOME / port / tmux server), driven over the flow WS, the HTTP siblings and the `th` binary; every wait polls with a timeout, never sleeps-and-hopes                                                               |
| **cmux** | XCUITests over the built app against a mock server and against a real backend                                      | `apps/smoothflow` XCUITests (th-a58a97) against `mock/server.mjs` and `flow_e2e_server` + `fake-claude` — see the macOS page                                                                                                                                |
| **cmux** | terminal-level assertions: what the pane shows                                                                     | `flow.snapshot` / `th flow snapshot` on the pane after every step; `flow.output` bytes on attach                                                                                                                                                            |

## The layers

| Layer          | Where                                             | Boots                                       | Proves                                                                           | Runtime                   |
| -------------- | ------------------------------------------------- | ------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------- |
| engine unit    | `crates/smooth-flow/src/*` (`#[cfg(test)]`)       | nothing (a live shell when tmux is present) | store, frames, hook table, reset parser, guard, backoff, manifests, scrape rules | ~3 s                      |
| route unit     | `crates/smooth-daemon/src/flow_route.rs`          | the axum router in-process                  | auth gate, WS hello/errors, hook long-poll over a socket, harness prefs routes   | ~2 s                      |
| **engine e2e** | `crates/smooth-daemon/tests/flow_e2e/`            | **a real `smooth-daemon` per test**         | everything below                                                                 | **~70 s wall** (24 tests) |
| macOS UI       | `apps/smoothflow/UITests`                         | the built app + `flow_e2e_server`           | the shell renders and drives the engine's states                                 | ~10 min (mac runner)      |
| phones         | `apps/smoothflow-mobile/{ios,android}` unit tests | nothing                                     | frame codec + reducer                                                            | seconds                   |

## The engine e2e suite

`cargo nextest run -p smooai-smooth-daemon --test flow_e2e` (nextest runs
each test in its own process, so each has its own daemon). Skips — or fails,
with `SMOOTH_E2E_STRICT=1`, which CI sets — when `tmux`, `bash`, `curl` or the
`th` binary is missing. `#![cfg(unix)]`: Windows compiles it empty.

| File           | Test                                                               | Asserts                                                                                                                                                                                                                                                                                                                                                                          |
| -------------- | ------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `shell.rs`     | `shell_lifecycle_new_attach_input_resize_snapshot_kill`            | `flow.new` → idle row (branch, socket, pid); attach streams only to attached clients; typed bytes echo twice; snapshot text + size follow attach and `flow.resize`; a second client; `flow.kill` → done, tmux session gone; input to a dead session is a `flow.error` with the client's `seq`                                                                                    |
|                | `shell_that_exits_is_done_and_a_failing_command_is_dead`           | `exit 0` → `done` with exit_code 0 (rule 5); `sh -c 'exit 7'` → `dead · crashed: exit 7`, never resumed; the list is newest-first; nothing streams for an unattached session                                                                                                                                                                                                     |
| `agent.rs`     | `agent_transitions_working_idle_needs_you_done`                    | pre-assigned uuid in argv, binary resolved through `prefer_paths`; the prompt's turn → unread idle via hooks with user/tool/system/agent `flow.event` lines; `mark_read`; `/perm` → `needs_you · permission` with a request_id, the hook held open; `flow.approve allow_session` → the agent prints the long-polled reply with the session rule; `/exit 0` → `done`, not resumed |
|                | `agent_question_notification_needs_you_and_steer_answers_it`       | a question notification → `needs_you · question` (no request_id); a steer answers it                                                                                                                                                                                                                                                                                             |
|                | `agent_usage_limit_is_scheduled_from_the_banner`                   | the banner → `limited` with `resume_at` at the next 11:59pm, source still `hooks`; the state holds                                                                                                                                                                                                                                                                               |
|                | `agent_usage_limit_resume_fires_when_the_window_passes` (slow)     | a banner ~1 min out → the supervisor presses Enter when it passes → `working`; the Enter reached the agent                                                                                                                                                                                                                                                                       |
|                | `agent_that_dies_is_resumed_with_its_session_id`                   | exit 2 → `starting · crashed: exit 2; resuming in 5s (attempt 1/3)`; relaunched with `--resume <id>` (argv, pane, same `agent_session_id`, new pid); the story in the event stream; hooks re-attach after the resume                                                                                                                                                             |
|                | `agent_that_keeps_crashing_is_dead_after_three_resumes` (slow)     | 5 + 10 + 20 s backoff, four launches (three with `--resume`), then `dead · gave up after 3 resumes`; no fourth                                                                                                                                                                                                                                                                   |
|                | `learned_session_id_binds_from_the_first_hook_and_kill_resume…`    | opencode/codex shape: no `--session-id`, the first hook from the worktree binds the id; `kill --resume` relaunches with `--resume <learned id>`                                                                                                                                                                                                                                  |
|                | `duplicate_resume_guard_holds_when_a_live_pid_owns_the_session`    | rule 4: with a live process owning the same harness session, a resume is `needs_you · held` naming the pid, the supervisor leaves it alone, and it resumes once the holder is gone                                                                                                                                                                                               |
|                | `native_harness_reports_its_own_turns`                             | th code shape: `turn_start` / `turn_end` through `event_map` → `native`; `ask` → needs_you answered by the keystroke path; `bye` + exit                                                                                                                                                                                                                                          |
|                | `scrape_harness_state_is_inferred_from_the_pane`                   | no hooks at all: working/idle/approval/limit scraped, source stays `inferred`; a scraped approval gets a `scrape-` request id and the menu key; `relaunch_command` reruns the original argv                                                                                                                                                                                      |
| `hooks.rs`     | `hooks_contract_per_event`                                         | every event Claude Code posts — SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, Notification (permission / question / other), PreCompact, SubagentStop, SessionEnd, unknown — with the state and the `flow.event` line each produces; unknown session → quiet `{}`                                                                                                |
|                | `permission_request_long_polls_until_approved_each_decision_shape` | deny / allow / allow_session reply bodies; a Notification keeps the request_id; a mismatched request_id does not resolve the poll                                                                                                                                                                                                                                                |
|                | `hooks_are_unauthenticated_and_everything_else_is_gated`           | 401 without the token on every route but hooks; `?token=`, Bearer, `X-Smooth-Token`; the WS handshake refuses a bad token                                                                                                                                                                                                                                                        |
| `cli.rs`       | `th_flow_json_against_the_live_daemon`                             | `ls` / `new` / `snapshot` / `send` / `inbox` / `approve` / `handoff` / `kill` `--json` shapes and the human lines; the two-line error contract; explicit argv after `--`                                                                                                                                                                                                         |
|                | `th_without_a_daemon_says_so_in_two_lines`                         | no `daemon.addr` → the hint; `th harness list` degrades to the files with a note                                                                                                                                                                                                                                                                                                 |
| `harnesses.rs` | `harness_matrix_state_source_per_manifest`                         | the built-ins are listed with their source + install reason; each fake-agent flavour ends a turn with the expected `state_source` (hooks / hooks / native / inferred); an unknown kind is refused with the pointer                                                                                                                                                               |
|                | `harness_prefs_sort_and_hide_reach_every_picker`                   | `PUT /api/flow/harnesses/prefs` → `flow.harnesses`, a fresh hello omits hidden, `th harness list` follows; `hide` / `unhide` / `order` verbs; unknown names refused; prefs persist                                                                                                                                                                                               |
|                | `th_harness_add_installs_a_custom_manifest_the_engine_launches`    | `th harness add <path>` validates and copies; refuses to clobber; an invalid manifest is refused; no daemon restart — the daemon lists it, `show` reads it, a session launches on it                                                                                                                                                                                             |
|                | `real_harnesses_launch_through_their_manifests` (opt-in)           | `SMOOTH_E2E_REAL_HARNESSES=1`: claude / opencode / codex where installed + `th code`, launched through their built-in manifests; argv[0] is the resolved binary, the launch shape per manifest, th code reports `native`                                                                                                                                                         |
| `isolation.rs` | `suite_never_writes_the_real_daemon_addr_or_token`                 | the user's `~/.smooth/{daemon.addr,daemon.lock,operator-token,flow.db}` are byte- and mtime-identical after a full boot + session; the rig's session is not on the default `smooth-flow` server                                                                                                                                                                                  |
|                | `daemon_teardown_leaves_no_tmux_server_or_process`                 | dropping the rig kills the daemon, its tmux server and the agent's process                                                                                                                                                                                                                                                                                                       |
|                | `flow_e2e_server_example_hosts_the_same_engine`                    | the macOS lane's host runs the same manifests + fake-agent to the same states                                                                                                                                                                                                                                                                                                    |

Measured on a loaded dev machine (load 25–40, other agents building):
**~70 s wall, 24 tests**, two of them "slow" (60 s and 66 s — the backoff and
usage-limit windows are real time, the engine has no test clock). CI runs
the whole suite on every PR; if it ever passes ~15 min on the runner, split
the two slow ones to a nightly `schedule:` — they are the only tests over
25 s.

### The rig (`support.rs`)

`Daemon::boot()` spawns `smooth-daemon operator --addr 127.0.0.1:0
--tmux-socket flow-e2e-<pid>-<n>` with a scrubbed environment:

| env                            | why                                                                                                                                      |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `HOME=<tmp>/home`              | `~/.smooth/{daemon.addr,operator-token,flow.db}` and `~/.smooth/harnesses/` are throwaway; `th` run with the same HOME finds THIS daemon |
| `SMOOTH_ALLOW_SECOND_DAEMON=1` | skip the machine-wide single-instance lock                                                                                               |
| `SMOOTH_TAILSCALE_SERVE=0`     | never re-point the tailnet `:443` at a test daemon (it happened once)                                                                    |
| `SMOOTH_RELAY=0`               | no relay dial-out                                                                                                                        |
| `SMOOTH_WORKSPACE=<tmp>/ws`    | a git repo (`main`, one commit) sessions run in                                                                                          |
| `PATH=<home>/.local/bin:$PATH` | `fake-agent` lives there; the manifests' `prefer_paths` resolve it under any PATH                                                        |
| no `LANG`, no `SHELL`          | on purpose — a launchd-started daemon has neither. This is how th-8e3087 found the tmux tab/locale bug below                             |

Port 0 is real: the daemon resolves the ephemeral port BEFORE the router
renders `{daemon_url}` (a bug this suite caught — hooks went to
`http://127.0.0.1:0`). `daemon.addr` under the test HOME is what `th flow`
and `th harness` read; `operator-token` is the `X-Smooth-Token`.

Waits: `wait_state` / `wait_until` / `wait_screen` poll every 250 ms with a
30 s cap (`WAIT`) and fail with the row, the pane, fake-agent's log and the
daemon log tail. `Ws::wait_for` drains frames until a predicate matches.
Dropping the rig SIGTERMs the daemon, kills its tmux server and prints the
daemon log when the test is panicking.

### fake-agent

`crates/smooth-daemon/tests/fixtures/fake-agent` (bash; needs `curl`). The
contract is in its header; the short form:

| Input                                                                                    | Behaviour                                                                                                                                                 |
| ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--session-id <id>` / `--resume <id>` / `--model <m>`                                    | as Claude Code; the first positional is a PROMPT of `;`-separated commands                                                                                |
| `$SMOOTH_URL` (the manifest's `{daemon_url}`)                                            | where hooks go; else `./.flow-e2e-addr`, else `$HOME/.smooth/daemon.addr`                                                                                 |
| `$FAKE_AGENT_MODE=hooks\|native\|scrape`                                                 | Claude Code events / th code events (`turn_start`, `turn_end`, `ask`, `bye`) / nothing posted, the pane is painted for the scraper                        |
| `./.fake-agent-session-id`                                                               | learned mode: the id to report (else `$FAKE_AGENT_SESSION_ID`, else fresh)                                                                                |
| `./.fake-agent-script`                                                                   | commands run on EVERY start, fresh and resumed — how a test makes each relaunch crash                                                                     |
| `/work <t>` `/perm` `/ask <t>` `/limit [time]` `/exit [code]` `/crash [code]` `/sleep n` | a turn; a permission (long-polled, decision printed); a question; the banner; SessionEnd + exit; exit with no hook; wait. Anything else is `echo: <line>` |
| `./.fake-agent.log`                                                                      | argv, every hook and its reply, every command — the assertions read it                                                                                    |

The four manifests in `tests/fixtures/harnesses/` are what a real manifest
looks like for each `[state] source` and `[launch] session_id` mode
(`fake-agent` = Claude shape, `-learned` = opencode/codex shape, `-native` =
th code shape, `-scrape` = a tool with no hooks). Copy one when you add a
harness — and run its flavour of the matrix test against it.

`fake-claude` (the macOS lane's stub, same directory) predates fake-agent and
is a strict subset of it; the mac lane keeps its own until that lane is
re-pointed (pearl filed).

## Bugs this suite found on day one

Recorded here because each is a class, not a one-off:

1. **tmux rewrites control characters under a non-UTF-8 locale.** The engine
   joined `#{pane_dead}\t#{pane_dead_status}` with a tab; with no `LANG`
   (launchd, CI, `env -i`) tmux printed `1_2`, the parser read "alive", and
   **no dead pane was ever detected** — the whole supervision story (resume,
   backoff, done/dead) was inert for any daemon not started from a shell.
   Fixed: `|`-joined formats + pure parsers (`tmux::parse_pane_dead`,
   `parse_pane_size`).
2. **`{daemon_url}` rendered from the requested address.** `--addr
127.0.0.1:0` gave every pane `SMOOTH_URL=http://127.0.0.1:0`. Fixed:
   `resolve_ephemeral_port` before the server is built.
3. **A resumed or prompt-less harness sat in `starting` for good.** Its
   `SessionStart` flipped the source to `hooks` (which stops the scraper) but
   mapped to nothing. Fixed: `SessionStart` on a `starting` row ⇒ `idle`.
4. **A `held` row flapped.** After the duplicate-resume guard parked a row,
   the next tick saw its dead tmux session and scheduled a resume; the guard
   refused it again; forever. Fixed: the supervisor skips held rows.

## Running locally

```sh
brew install tmux                          # bash + curl ship with macOS
CARGO_TARGET_DIR=$HOME/.cargo/target-e2e \
  cargo nextest run -p smooai-smooth-daemon --test flow_e2e        # ~70 s
# one test, with the daemon log on failure:
CARGO_TARGET_DIR=$HOME/.cargo/target-e2e \
  cargo nextest run -p smooai-smooth-daemon --test flow_e2e agent_that_dies --no-capture
# the real CLIs installed here, through their manifests:
SMOOTH_E2E_REAL_HARNESSES=1 cargo nextest run -p smooai-smooth-daemon --test flow_e2e real_harnesses
```

`th` is found next to the daemon binary (a workspace build puts both in
`target/debug/`), or via `SMOOTH_TH_BIN`. Never `pnpm install:th` for this —
the suite must not depend on, or replace, the `th` on your PATH. The rig
never reads or writes your `~/.smooth`; `isolation.rs` proves it every run.

## CI

`pr-checks.yml`, the `rust` job, Linux leg: `tmux` is installed with the
other system packages and `SMOOTH_E2E_STRICT=1` makes a skip a failure. The
suite runs inside the normal `cargo nextest run --profile ci`; nextest builds
`th` and `smooth-daemon` first, and the `cargo build --examples` step builds
`flow_e2e_server` for the example test. Windows compiles the crate empty.

## Adding a scenario

1. Boot a rig: `let d = Daemon::boot().await;` after `if !prereqs() { return; }`
   (`prereqs_with_th()` when the test runs `th`).
2. Drive it the way a client would — `d.ws()` for frames, `d.post` /
   `d.get` for the HTTP siblings, `d.th(&[…])` for the CLI, `d.hook(…)` for
   what a hook script posts. Steer fake-agent with `d.send(id, "/work x")`.
3. Assert with `wait_state` / `wait_until` / `wait_screen`; read
   `d.agent_log()` for what the agent saw. Never `sleep` and check.
4. A new harness shape = a new manifest in `tests/fixtures/harnesses/` and a
   row in `harness_matrix_state_source_per_manifest`.

## Gaps (pearls)

- The two slow tests wait real backoff/limit windows; an engine test clock
  would cut ~2 min of CI to seconds.
- Fan-out (`flow.fanout.new` / `pick`) is unit-tested only; an e2e needs
  `th pearls` in the rig's HOME.
- The real-harness launches are opt-in and creds-free: a nightly on a
  runner with Claude Code / opencode / codex installed would make them a gate.
- `fake-claude` → `fake-agent` in the macOS lane.
