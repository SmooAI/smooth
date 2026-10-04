# SmoothFlow — the flow engine and its protocol

> Epic th-6ac036 · lane A (engine + `th flow`) th-7f0af3. The v0 wire contract
> below is the one every other lane (macOS app, phones, hook scripts) codes
> against; change it only by agreement with the engine lane.

SmoothFlow is Big Smooth's session manager: agents (Claude Code, Codex,
OpenCode) and plain shells run under one long-lived tmux server, their PTY
bytes stream to whichever surface is looking (desktop, `th flow attach`, a
phone), harness hooks drive their state, and a supervision tick keeps them
alive across crashes and usage limits. The engine — the `smooth-flow` crate,
hosted inside `smooth-daemon` — is the **only state holder**. Every shell is a
dumb view.

> **Clients:** every SmoothFlow client (Mac, the Linux/Windows desktop app, iOS, Android) is built from
> [SmoothFlow-Client-Spec.md](SmoothFlow-Client-Spec.md): layout, surfaces, keymap, terminal requirements
> and the conformance vectors that keep them identical.

## Where the pieces live

| Piece                                                      | Path                                                                                 |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Engine crate (store, tmux glue, PTY, supervision)          | `crates/smooth-flow/`                                                                |
| Session host seam (`SessionHost`, `TmuxHost`)              | `crates/smooth-flow/src/host.rs` — see [host](#session-host)                         |
| Engine-owned PTY host (`flow-host`) IPC (ADR-011)          | [SmoothFlow-Session-Host.md](SmoothFlow-Session-Host.md) (host built, th-e4aef9)     |
| Daemon transport (`/api/flow/*`, WS, hooks long-poll)      | `crates/smooth-daemon/src/flow_route.rs`                                             |
| Relay routing of `channel:"flow"` envelopes + phone caps   | `crates/smooth-daemon/src/relay.rs`                                                  |
| End-to-end encryption + phone pairing (th-d98fde)          | `crates/smooth-daemon/src/flow_e2e.rs`, `flow_pair_route.rs`                         |
| Shared pane-state heuristics (moved from `th claude`)      | `crates/smooth-tmux/src/detect.rs`                                                   |
| CLI                                                        | `crates/smooth-cli/src/flow.rs` (`th flow …`)                                        |
| Big Smooth's in-process `flow_*` tools (th-8b3918)         | `crates/smooth-daemon/src/flow_tools.rs` — see [fleet](#big-smooth-drives-the-fleet) |
| Structured diffs, turn snapshots, hunk actions (th-26f5b9) | `crates/smooth-flow/src/diff/` — see [SmoothFlow-Diff.md](SmoothFlow-Diff.md)        |
| Session store                                              | `~/.smooth/flow.db` (SQLite, WAL; `$SMOOTH_FLOW_DB`)                                 |
| tmux server                                                | `tmux -L smooth-flow` — see [tmux socket](#tmux-socket-tcc)                          |

## Process model

```
Big Smooth.app / th up ──► smooth-daemon ──► smooth_flow::Engine
                                              │
                                              ├── tmux -L smooth-flow   (outlives the daemon)
                                              │     ├── fs-1a2b3c4d: sh -c 'trap : INT QUIT; claude --session-id <uuid> …; c=$?; …; exit $c'
                                              │     └── fs-9e8f7a6b: sh -c 'trap : INT QUIT; zsh -l; c=$?; …; exit $c'
                                              │
                                              └── per attached session: portable-pty ⟷ `tmux attach -t fs-…`
                                                                          │
      th flow attach / macOS app / phone  ◄── GET /api/flow/ws ◄──────────┘  flow.output {data_b64}
```

- **Sessions run under tmux, not under the daemon.** A daemon restart or an
  app crash never kills a PTY: the engine re-opens `flow.db`, finds the tmux
  session by name (the flow session id) and carries on. `remain-on-exit` is on
  so a dead pane stays until the engine has read how it ended.
- **tmux is found off-`PATH`, and panes get the login `PATH` (th-9f6814).** An
  app launched from Finder gives its daemon `PATH=/usr/bin:/bin:/usr/sbin:/sbin`,
  which holds neither Homebrew's tmux nor the user's CLIs. `smooth_tmux::tmux_bin`
  resolves tmux once: `$SMOOTH_TMUX_BIN` (an override; a broken one is an error,
  not a fallback), then `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin`,
  `/opt/local/bin`, `/usr/bin`. Every tmux call in `smooth-flow`, `smooth-tmux`
  and `th claude` goes through it, `tmux attach` included. Every launch exports a
  pane `PATH` (`smooth_flow::pane_path`): the daemon's non-system entries, then
  the user's `$SHELL -l -i` `PATH` (captured once, 5 s timeout;
  `SMOOTH_FLOW_LOGIN_PATH=0` skips it), then `/usr/bin` and the other system dirs,
  then the well-known tool dirs. Harness binaries resolve against that same
  `PATH`. The daemon logs the resolved tmux at boot, and `th harness doctor`
  prints it.
- **A launch that fails says why.** No tmux, a tmux error, or a missing harness
  binary sends the row to `dead` with attention `launch_failed` and a detail
  such as "tmux not found (looked in PATH, /opt/homebrew/bin, …): install it
  with `brew install tmux`". The failure is logged at WARN and broadcast as
  `flow.session`. Before this, the row sat in `starting` with no pid and no
  detail, and nothing was logged.
- **A wrapper in the pane records the exit code (th-7ff336).** tmux knows a
  pane's `#{pane_dead_status}` only once its server has reaped the process.
  On Linux that lagged seconds, and tmux sometimes misses the SIGCHLD
  entirely, long enough for a clean exit 0 to be resumed as a crash. So the
  launch line runs the agent as a CHILD of a small `sh`
  (`tmux::wrapped_command_env`). The wrapper writes the agent's `$?` to
  `flow-exit-status/<id>.<pane pid>.exit` (temp file + rename) and exits with
  that same code, so tmux and the file agree. `trap : INT QUIT` is a no-op
  handler, and handlers reset across `exec`, so Ctrl-C still reaches the
  agent while the wrapper survives it and keeps waiting. The pane never goes
  dead under a live agent. `trap '' INT` would be inherited as ignored. A
  non-interactive `sh` has no job control, so the agent stays in the wrapper's
  process group, the one `kill_tree` signals.
- **The pane pid is the wrapper's.** The engine records `pid` + start time (from
  `ps -o lstart=`) as the liveness index, so a recycled pid can't pass for the
  agent. The wrapper lives exactly as long as it waits on the agent. The file
  name carries that pid, and launch, kill and close clear a session's files,
  so an earlier launch's code is never read.
- **How an exit settles** (`settle_exit`): tmux's status or signal (after one
  `run-shell true` nudge, which makes tmux collect a missed child), else the
  wrapper's file, else, after 3 s, an explicit **unknown**. There is never a
  fake `-1`. Unknown is shown as "exit status unknown" and is **never
  auto-resumed**, since the agent may have quit on purpose. `128 + n` is shown
  as "killed by signal n" and keeps its code in the row. The wait marks the
  session and returns, and the next tick re-checks, so the supervisor never
  blocks.
- **Streaming is a tmux client on a PTY, not `pipe-pane`.** `pipe-pane` yields
  bytes positioned for the pane's own geometry and only from the moment the
  pipe opens, so a late-joining client gets no initial screen. A `tmux attach`
  client on a `portable-pty` gets a full redraw on attach, follows the PTY's
  size (`window-size latest`), and passes keystrokes through untouched —
  lossless bytes + resize, which is what a terminal-emulator surface needs. The
  PTY exists only while ≥1 client is attached; the last `flow.detach` drops it
  and the tmux session keeps running detached.
- **One bridge per session, shared by every attached client (th-6d8f84).** The
  lookup, the spawn on a miss, the insert and the client count happen in one
  critical section on the bridge map. Two clients attaching at once (the Mac
  app and the phone, two windows, a relay reconnect) get one `tmux attach`
  client, and an EOF only evicts the bridge it belongs to (bridges carry a
  generation). One bridge means one geometry: the latest attach or resize wins,
  as with tmux's `window-size latest`, so the phone and the Mac take turns
  rather than sizing to the smaller one (th-87cbca).
- **Disposing a bridge must never type into the pane.** `portable-pty`'s unix
  writer writes `\n` + `VEOF` into the PTY when dropped. Our child is a
  raw-mode tmux client, so those arrive in the pane as a blank line and `^D`,
  and a login shell at its prompt prints `logout` and exits. `close()` only
  SIGHUPs the client (tmux prints `[lost tty]`), so a drop right after it raced
  the signal: a plain last-client detach could end the user's shell. The writer
  is held by the reader thread and released only after `child.wait()`, when
  there is no client left to forward the bytes.

### The session host seam (th-64d4ab) {#session-host}

The engine never calls tmux itself. It holds an `Arc<dyn SessionHost>`
(`crates/smooth-flow/src/host.rs`) and asks it for everything a session
needs:

- launch, with the exit-code wrapper;
- liveness, pid and pane meta;
- exit status, and the wrapper's recorded code;
- size, capture, paste, send text and send key;
- kill the session or the whole server;
- the attach stream;
- process liveness and tree kill.

A session is addressed by a `SessionRef { socket, name }`: the namespace the row was
launched in plus the flow session id. The host's `default_socket()` is also
the ownership identity supervision filters on (th-4f7866).

`TmuxHost` is the default, set by `EngineConfig::new`. Every method delegates
unchanged to `tmux.rs`, `pty.rs` and `proc.rs`, so everything above in this
section still describes what runs. Two things stay in the engine as plain
helpers rather than host calls, because they are data rather than host
operations: `PaneExit::describe`'s `tmux::signal_name` (this platform's
signal table) and the `PaneDeath` / `PaneMeta` types, which now live in
`host.rs`.

The seam has two uses:

- **A native host.** `PtyHost` replaces tmux everywhere, not only on Windows
  ([ADR-011](../Decisions/ADR-011-smoothflow-engine-owned-ptys.md), epic
  th-ce4f88). A per-session `smooth-daemon flow-host` process owns the PTY and a
  headless libghostty-vt, and attach becomes a `flow.replay` snapshot followed
  by sequenced `flow.output` ([Replay](#replay)). The daemon ⇄ host protocol
  is [SmoothFlow-Session-Host.md](SmoothFlow-Session-Host.md). Supervision
  won't change. The host process is built
  (`crates/smooth-flow/src/session_host/`, th-e4aef9); `PtyHost` (th-dc9822)
  is next.

    Its headless terminal is `crates/smooth-flow-vt` (th-5025fb): a safe
    wrapper around libghostty-vt, built from the same pinned
    `scripts/ghostty-vt/` script and lock as SmoothFlow Desktop. It covers what
    tmux did for the engine: feed bytes, read the visible screen as text
    (`capture-pane`), `alternate_on`, the cursor, the OSC title, bracketed paste
    and DECCKM, and encode manifest key names (`Enter`, `C-c`, `Down`) and
    pastes for the program's current input modes. It also makes the bounded
    `flow.replay` snapshot. While a TUI holds the alternate screen, that
    snapshot still carries the primary screen's history, cached at the instant
    the TUI came up. The crate's module docs give the bounding policy and the
    known libghostty-vt limits. `smooth-flow` takes it only behind the
    `pty-host` feature (off by default) until `PtyHost` lands, so nothing that
    ships today needs Zig to build.

- **Engine tests without tmux.** `host::fake::FakeHost` (test-only) keeps
  sessions in memory and records every call. With it, tests cover the
  supervisor against states a real pane only produces by timing: a pane
  dead but unreaped, a wrapper record winning over tmux's silence, and a
  vanished agent resumed with its manifest's resume argv. These ran as
  live-tmux tests, Linux-only for the unreaped case, or not at all.

## Transport

- **Own WebSocket** — `GET /api/flow/ws` on the daemon, mounted through the
  operator's `serve_routes` seam. The daemon cannot intercept the engine's
  canonical `/ws`, so flow is a sibling channel with the same origin and the
  same `~/.smooth/daemon.addr` discovery.
- **HTTP siblings** — `GET/POST /api/flow/sessions`, the per-session
  `POST …/{id}/input`, `…/resize`, `…/approve`, `…/kill`, `POST /api/flow/hooks`,
  and `GET /api/flow/sessions/{id}/handoff`. Additive to the spec:
  `POST …/{id}/send` and `GET …/{id}/snapshot`, so `th flow` needs no WS for
  one-shots.
- **Auth (additive to the v0 spec).** Every flow route except `/api/flow/hooks`
  requires the daemon's local token — `?token=`, `Authorization: Bearer`, or
  `X-Smooth-Token`. A flow session is a shell on this machine and the daemon
  may be reachable over a tailnet, so the gate is not optional. Clients already
  carry the token for the operator WS. Hooks authenticate per launch instead;
  see [Hook authentication](#hook-authentication-th-91d032).
- **Relay (phones).** The envelope stays `{to, frame}`. A frame carrying
  `"channel":"flow"` is bridged to `/api/flow/ws` by a second per-phone
  loopback bridge; no `channel` ⇒ operator WS, unchanged. Outbound
  `flow.output` to a phone is coalesced to ~30 fps and split into ≤16 KiB
  frames (`relay::OutputCoalescer`); phones never receive raw scrollback.
  A **paired** phone's frames are end-to-end encrypted — see
  [End-to-end encryption](#end-to-end-encryption).
- Every frame is one JSON object `{"channel":"flow","type":"<name>", …}`.
  Unknown types are ignored, never fatal. Errors are objects, never strings.

## Session model

```
sessions(id TEXT PK,             -- "fs-" + 8 hex
         kind, title, project, worktree, branch, pearl_id,
         agent_session_id,       -- pre-assigned `claude --session-id` uuid
         argv,                   -- JSON array actually launched
         tmux_session, pid, pid_start,
         state,                  -- starting|working|idle|needs_you|limited|done|dead
         attention,              -- JSON {reason, detail, resume_at, request_id}
         fan_out_id, created_at, updated_at, ended_at, exit_code, unread,
         tmux_socket,            -- the `tmux -L` server the session lives on (v0.1)
         state_source)           -- hooks | native | inferred (th-5c5457 / th-0f6126)
fan_outs(id TEXT PK, prompt, base_commit, pearl_id, created_at, winner_session_id)
events(session_id, seq, at, kind, text, PK(session_id, seq))  -- last 200 per session (v0.1)
config(key TEXT PK, value)      -- harness_prefs = {order, hidden} (th-0f6126)
```

Timestamps are UTC RFC3339 text written from Rust `Utc::now()`. Terminal
states (`done`, `dead`) are sticky; every live state may move to any other
(hooks and pane scrapes arrive out of order, so a strict ladder would drop
real signals).

## Frames

Engine → clients (broadcast; `flow.output` and `flow.replay` only to clients
that attached the id): `flow.hello`, `flow.session`, `flow.session.removed`,
`flow.output`, `flow.screen`, `flow.attention`, `flow.fanout`, `flow.error`,
(v0.1) `flow.event`, `flow.handoff`, (th-0f6126) `flow.harnesses`,
(th-26f5b9) `flow.diff.changed` — plus the direct replies `flow.diff` and
`flow.diff.result` — and (th-c61966) `flow.replay`, see
[Replay](#replay).

Clients → engine: `flow.attach`, `flow.detach`, `flow.input`, `flow.resize`,
`flow.snapshot`, `flow.new`, `flow.send`, `flow.approve`, `flow.kill`,
`flow.fanout.new`, `flow.fanout.pick`, `flow.mark_read`, (v0.1)
`flow.hello`, `flow.handoff`, (th-e126cc) `flow.close`, and (th-26f5b9)
`flow.diff`, `flow.diff.revert`, `flow.diff.stage`, `flow.diff.unstage`,
`flow.diff.review` — see [SmoothFlow-Diff.md](SmoothFlow-Diff.md).

Field-level shapes are the types in `crates/smooth-flow/src/protocol.rs`
(`ClientFrame`, `ServerFrame`), which the round-trip tests pin. One additive
field: `flow.fanout.new` accepts `project` (the main checkout to fan out from;
defaults to the daemon's workspace).

## Replay and per-session `seq` (th-c61966) {#replay}

> Specified for the engine-owned PTY host
> ([ADR-011](../Decisions/ADR-011-smoothflow-engine-owned-ptys.md); the
> daemon ⇄ host protocol is [SmoothFlow-Session-Host.md](SmoothFlow-Session-Host.md)).
> Additive: nothing here changes what an old client or an old engine does.

A client attaching to a session gets a **snapshot of the terminal** followed
by **sequenced live bytes**, instead of tmux's redraw. The snapshot carries
the session's scrollback, so client-side scrollback works.

### The frame

```json
{
    "channel": "flow",
    "type": "flow.replay",
    "id": "fs-1a2b3c4d",
    "cols": 120,
    "rows": 40,
    "seq": 812,
    "data_b64": "G1s/MTA0OWgb…",
    "reason": "attach",
    "part": 0,
    "parts": 1
}
```

- `data_b64` is a VT byte stream (libghostty-vt's formatter output: screen,
  history, cursor, SGR, modes, scrolling region, charsets, keyboard modes)
  that rebuilds the session's terminal when fed to a **fresh** terminal of
  `cols`×`rows`. It is current through output `seq`: it contains the effect
  of every `flow.output` with `seq` ≤ this one and none after.
- It is bounded by a byte budget, and degrades in this order
  (`smooth_flow_vt::Fidelity`): everything (screen and all history); else the
  full screen plus the newest history rows that fit; else the visible
  screen's plain text and cursor (no colours, no modes); else nothing. An
  empty replay is still a reset, so the client shows a blank screen, never a
  corrupt one, and the live stream carries on from there.
- On the alternate screen the replay also carries the primary screen and its
  history (cached when the TUI came up), so leaving the TUI shows them. The
  alt screen gets the budget first and the cached primary the rest, by the
  same order.
- Rows go out as laid out: soft wraps are not rejoined. A client never
  reflows old wraps itself, because every resize is followed by a fresh
  replay at the new size.
- `data_b64` may be empty. That is still a replay: reset, and what follows
  draws the screen (a tmux-host session, or a snapshot where nothing fit).
- `reason` is `attach`, `lag`, `resize` or `host` (the session moved to a new
  host process, e.g. Kill & Resume). It is informative; clients treat every
  replay the same.
- `part` / `parts` are present only on a chunked replay (the relay, below).
  Absent means part 0 of 1.

### `seq` is per session

`flow.output.seq` is a property of the **session**, not of the connection or
the bridge:

- On the `pty` host it is the host's output counter. It survives daemon
  restarts, WS reconnects and relay reconnects, and every client attached to
  the session sees the same `seq` for the same bytes. A relaunch continues it
  (`seq_start`).
- It rises by at least 1 per engine output frame. The relay's coalescer
  (below) keeps the engine's `seq`: a merged run carries the `seq` of its
  last engine frame, and the chunks of one run **share** that `seq`. So on
  the wire a session's `seq` is non-decreasing, and clients never compare one
  output's `seq` with another's, only with the replay's.
- On the tmux host `seq` stays what it is today, a counter per bridge.

### Engine rules

1. **On `flow.attach`** from a client that declared `replay: true`, the engine
   subscribes the client to the session's output first, then takes a
   snapshot, then sends `flow.replay`. Outputs the snapshot covers may already
   be queued on either side of it; the client sorts that out by `seq`.
2. **On lag.** When a WS subscriber falls behind the broadcast
   (`RecvError::Lagged`, today a silent drop), the engine sends that
   subscriber a fresh `flow.replay` (`reason: "lag"`) for every session it has
   attached, before any further output. The host's `overrun` does the same
   for every attached client.
3. **On every resize.** Every `flow.resize`, and every `flow.attach` that sets
   the session's size, is followed by a `flow.replay` at the new size to
   **every** client attached to the session (one geometry per session, latest
   wins, as now). Resizes within 50 ms coalesce into one replay. The snapshot
   keeps rows as laid out, so this replay, not the client's own reflow, is
   what the client ends up showing.
4. **On a new host** (Kill & Resume, crash resume), every attached client gets
   `flow.replay{reason:"host"}`.
5. **Legacy attaches.** A `flow.attach` without `replay: true` gets the same
   snapshot as an ordinary `flow.output` with `seq` = the snapshot's and
   `ESC c ESC[3J` (reset, clear history) before the snapshot bytes. An old
   client therefore sees the screen on a `pty` session and still works.
6. **tmux-host sessions** get an empty `flow.replay` on a `replay: true`
   attach, and the engine then **forces a full redraw** on the bridge's tmux
   client (`tmux refresh-client -t <that client>`), so the client always ends
   with the current screen. This matters when the attach joins an existing
   bridge (a second client, a relay reconnect): with no redraw, the reset
   would leave it blank until the pane next changed. The replay's `seq` is
   the bridge's last `seq` before the refresh is requested, so the redraw
   arrives as newer output. The client's logic is the same for both hosts.
7. **Terminal queries are answered by the host.** On the `pty` host the
   session's headless VT answers device-attribute and status queries (DA,
   DSR, …): the host writes `Vt::take_replies` back to the PTY. Clients
   therefore must not answer them too (see Client rules).

### Client rules

Normative text, and the shared logic every client replays, are in the
[Client Spec §10](SmoothFlow-Client-Spec.md#10-terminal-requirements-all-clients)
and `spec/vectors/replay.json`. In short:

- Expect a replay when `flow.hello.capabilities` contains `"replay"` and you
  attached with `replay: true`. Until it arrives, **buffer** output.
- On a complete replay: reset to a fresh `cols`×`rows` terminal (screen,
  history, modes, selection, scroll position), feed the data, then apply the
  buffered outputs whose `seq` is greater than the replay's, dropping the
  rest.
- After that, drop any output whose `seq` ≤ the latest replay's and apply the
  others in arrival order. The **latest** replay is always the baseline, even
  if its `seq` is lower than the one before it.
- **Never render a partial replay.** A gap in `part` (or a malformed one)
  means re-send `flow.attach` and wait for a new replay.
- **Don't answer terminal queries on a `pty` session.** A client's terminal
  generates replies to DA/DSR and similar queries in the output it parses.
  For a session whose `host` is `pty`, the client must discard them and never
  send them as `flow.input`; the host has already answered, and a second
  answer reaches the program as stray input. For `tmux` sessions (and rows
  without `host`, from older engines) clients keep today's behaviour.

### Capability

- `flow.session` gains `host`: `"pty"` or `"tmux"` (absent from older
  engines, which means tmux). It is the row's `host` column (ADR-011
  §Migration) and decides the query rule above.
- `flow.hello` gains `capabilities: ["replay"]` (an array of strings; absent
  on older engines, which means none). A client that sees no `replay` keeps
  today's behaviour: apply outputs in arrival order and expect no replay.
- `flow.attach` gains `replay: true` (default `false`) and an optional
  `replay_max_bytes` (the client's budget for one replay; the engine clamps it
  to 64 KiB … 4 MiB, default 1 MiB).
- A client must still accept a `flow.replay` it didn't ask for, and must
  ignore one for a session it hasn't attached.

### Relay (phones)

- The relay bridge sets `replay_max_bytes` to at most **256 KiB** on a phone's
  `flow.attach` before it reaches the engine.
- The phone-side `OutputCoalescer` keeps the engine `seq` (it no longer
  numbers frames itself; see above).
- A `flow.replay` larger than 16 KiB of data is split into parts of at most
  16 KiB each: identical frames except `data_b64`, with `part` = 0 … `parts`−1
  and `parts` the total. The coalescer first **discards** any output it is
  holding for that session (all of it is covered by the replay), then sends
  every part, in order and back to back (not paced by the 30 fps tick), and
  sends no output for that session between parts. Each part is sealed as an
  ordinary end-to-end encrypted data frame.
- A phone buffers parts until it has all of them; a new `part: 0` discards an
  incomplete set. It renders nothing from a partial replay.

### Snapshot vs replay

`flow.screen` (the reply to `flow.snapshot`) stays what it is: plain text of
the visible screen for thumbnails, the fleet list and Big Smooth's tools. It
never feeds a terminal. On the `pty` host it comes from the host's `screen`
query instead of `capture-pane`.

## v0.1 additions (th-d33afa) — what the phones needed

All additive; a v0 client that ignores unknown types is unaffected.

### `flow.event` — the per-session event stream

`{channel:"flow", type:"flow.event", id, event_id, at, kind, text}` with
`kind ∈ user | agent | tool | system` — the Chat tab of the companion apps.
`event_id` is `<session id>-<seq>` (monotonic per session). The engine keeps
the last **200** per session in `flow.db` (`events`) and **replays them on
`flow.attach`**, before any `flow.output`, so a phone that just looked sees
what happened while it wasn't. Derivation (`protocol::hook_event_text` +
the engine):

| Source                                                  | kind     | text                                                                                                                         |
| ------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------- |
| hook `UserPromptSubmit` (`prompt`)                      | `user`   | the prompt                                                                                                                   |
| `flow.send {text}`                                      | `user`   | the steer                                                                                                                    |
| `flow.approve {decision}`                               | `user`   | `approve: allow` / `deny` / `allow_session`                                                                                  |
| hook `PreToolUse`                                       | `tool`   | `● Bash(ls)` — tool + the command/path/pattern                                                                               |
| hook `Stop` (`last_assistant_message`)                  | `agent`  | the agent's final message                                                                                                    |
| hook `Notification` (`message`)                         | `system` | the message                                                                                                                  |
| hook `SessionEnd` (`reason`)                            | `system` | `session ended (reason)`                                                                                                     |
| any **state change** (hooks, scrape, supervision, kill) | `system` | `working` · `idle` · `needs_you · permission: Bash: rm x` · `limited · usage_limit: resumes at …` · `dead · crashed: exit 1` |

A `PermissionRequest` adds no line of its own — its `needs_you` state change
carries the detail, so the prompt isn't reported twice. `PostToolUse`
produces no line either (the state stays `working`).

### `flow.handoff` over WS

`flow.handoff {id}` → `flow.handoff {id, pearl, handoff, checkpoints, blocks,
pr}` — the same body as `GET /api/flow/sessions/{id}/handoff` (below) plus
the session `id`, because the relay brokers WS only. Nulls are sent, never
omitted, so a phone can tell "no pearl" from "field missing".

### Client `flow.hello`

A client may send `{channel:"flow", type:"flow.hello"}`; the engine answers
with a fresh `flow.hello`. This is the phone's bridge nudge: over the relay
nothing opens the flow WS until the phone sends a frame, so the bridge is
opened by the **first** `channel:"flow"` envelope (whatever its type), the
engine's on-connect hello goes back, and the nudge's reply follows.

### `flow.close` — close the pearl, GC the worktree (th-e126cc)

`flow.close {id, close_pearl, remove_worktree, force}` (all flags default
off) finishes a session for good: a live one is killed first; then
`th pearls close <pearl>` runs in the project when `close_pearl` and the row
has a pearl; then, when `remove_worktree`, `git worktree remove` + the branch
is deleted — but only once the branch is **merged** into the project
(an ancestor of its HEAD, or a merged PR per `gh`, since the repos
squash-merge) and the worktree is clean; the main checkout is never removed.
Then the row is dropped and `flow.session.removed` is broadcast. A dirty or
unmerged worktree is refused as `flow.error` with **nothing touched**;
`force` removes it anyway. HTTP sibling: `POST
/api/flow/sessions/{id}/close` with the same body, replying
`{id, pearl_closed, worktree_removed, branch_deleted}` (nulls for what was
not done). CLI: `th flow close <id> [--keep-pearl] [--keep-worktree]
[--force]` — the CLI defaults both actions **on**.

### tmux socket (TCC) {#tmux-socket-tcc}

Sessions are created on the tmux server named by, in order: the
`tmux_socket` field of `flow.new` (`th flow new --tmux-socket <name>`), then
`smooth-daemon operator --tmux-socket <name>` / `$SMOOTH_FLOW_TMUX_SOCKET`,
then the default `smooth-flow`. Every row records its socket, so a daemon
restarted with a different setting still finds its old panes.

**Ownership (th-4f7866).** Each row also records its `owner`: the socket
name the _creating_ daemon was configured with — that daemon's identity
across restarts, distinct from where the pane lives. A daemon's supervision
tick only touches rows it owns (rows from before the column: the daemon
whose socket matches the row's). Two daemons sharing one `flow.db` — `th
up`'s on the default socket and the SmoothFlow app's child on `smoothflow`,
or an orphaned instance — otherwise each looked for the other's panes on
_its_ server, marked them `dead · process vanished`, and raced to relaunch
them. Attach, send, kill and snapshot are not ownership-gated: they use the
row's socket, so `th flow` drives any session through any daemon. The app
additionally keeps its own db (`SMOOTH_FLOW_DB=~/.smooth/smoothflow-flow.db`).

**Why it matters:** on macOS, TCC attributes a pane's grants (Full Disk
Access, Calendar, Notifications) to the process that started the tmux
_server_. The SmoothFlow app starts `tmux -L smoothflow` itself (as a direct
child, on every launch) and passes `SMOOTH_FLOW_TMUX_SOCKET=smoothflow` to
the daemon it spawns. A session created on any other socket — the default
`smooth-flow` server, or one a terminal started — runs agents with **no**
FDA/Calendar access, and the failures are silent (empty listings, "not
authorized" from EventKit), not prompts. `th flow new` from a terminal
therefore lands on the app's server only with `--tmux-socket smoothflow`.

## End-to-end encryption {#end-to-end-encryption}

> Pearl th-d98fde. The relay (`rust/relay-ws` in smooai) forwards `{to, frame}`
> opaquely and never inspects `frame`, so nothing in the relay changed. The
> reference implementation is `crates/smooth-daemon/src/flow_e2e.rs`; the
> Swift (`SmoothRelay/FlowCrypto.swift`) and Kotlin (`relay/…/FlowCrypto.kt`)
> ports in smooai `apps/bigsmooth/relay` assert the same fixture,
> `crates/smooth-daemon/tests/fixtures/flow-e2e-v1.json`.

Terminal bytes must not be readable by the relay. A phone therefore **pairs**
with a daemon once, out of band, and from then on every `channel:"flow"` frame
between them is sealed; only the envelope stays routable. Big Smooth chat
frames (no `channel`) are untouched.

**Primitives.** X25519 (with the contributory check), HKDF-SHA256,
ChaCha20-Poly1305 with a 12-byte nonce `[direction, 0, 0, 0, u64 counter BE]`
(direction `0` = phone→daemon, `1` = daemon→phone; counters start at 1) and the
constant AAD `smoothflow-e2e/v1`. Keys and the code are base64url (unpadded);
`ct` and salts are standard base64. RustCrypto on the daemon, CryptoKit on
iOS, Tink's pure-Java subtle primitives on Android (X25519 / ChaCha20-Poly1305
in `javax.crypto` need API 33 / 28, and the app ships at minSdk 26).

**Pairing** (once per phone; `POST /api/flow/pair` mints it, Settings ▸
Phones and `th flow pair --qr` show it, `GET /api/flow/pair/{id}` polls it):

1. The daemon makes a fresh X25519 keypair, a 128-bit one-time code and an
   8-hex pairing id, and shows
   `smoothflow://pair?v=1&p=<id>&d=<daemon device>&k=<daemon pub>&c=<code>&l=<label>`
   as a QR. The link is valid for 5 minutes. The code never crosses the relay.
2. The phone scans it (in-app camera, the Camera app via the URL scheme, or
   pasted), makes its own X25519 keypair and derives
   `pairing_key = HKDF(salt = code, ikm = X25519(phone_sk, daemon_pk), info = "smoothflow-pair/v1" || id)`.
3. The phone sends, through the relay,
   `{channel:"flow", v:1, type:"flow.pair", pair:<id>, pk:<phone pub>, n:1, ct}`
   where `ct` seals `{"type":"flow.pair.hello","label":"Brent's iPhone","platform":"ios"}`
   under the pairing key (direction 0, n = 1). Being able to seal under a key
   that needs the code is what authenticates the phone: the relay sees both
   public keys but cannot substitute its own.
4. The daemon derives the same key, opens the hello, writes the pairing to
   `flow.db` (`pairings(device PK, label, platform, public_key, key_hex,
created_at, last_seen_at)`, keyed by the phone's relay device id) and
   answers `{channel:"flow", v:1, type:"flow.pair", n:1, ct}` sealing
   `{"type":"flow.pair.ok", device, label, protocol:1}` (direction 1, n = 1). The
   phone stores the pairing key in the Keychain / its Tink-encrypted store.
   A failed scan answers a plaintext `flow.error {code:"pair_failed"}` and
   leaves the QR valid.

**Sessions** (every connection). The pairing key never seals data. On connect
the phone sends `{channel:"flow", v:1, type:"flow.e2e.open", salt:<16 B>}`;
the daemon answers the same shape with its own 16-byte salt and both derive
`session_key = HKDF(salt = phone_salt || daemon_salt, ikm = pairing_key, info = "smoothflow-session/v1")`.
Data frames are then `{channel:"flow", v:1, n, ct}` — the plaintext is the
ordinary flow frame JSON, `flow.output` included (after the phone caps). The
daemon's salt is what stops a recorded session from being replayed after a
restart; each receiver also requires a strictly increasing `n` and never
advances on a failed authentication. Engine frames that arrive before the
session is open (the on-connect `flow.hello` races the phone's open) are
buffered, up to 64, and flushed sealed once it is.

**Rejections are visible.** Plaintext from a paired phone is answered with
`flow.error {code:"e2e_required"}` and not forwarded; a data frame from a
revoked phone gets `e2e_revoked`, one before `open` gets `e2e_not_open`, one
that fails authentication or replays gets `e2e_bad_frame`, and an `open` from
an unpaired phone gets `e2e_not_paired`. Unpaired phones keep working in
plaintext (the pre-pairing apps) unless the daemon runs with
`SMOOTH_FLOW_E2E_REQUIRED=1`. Re-pairing a device rotates its key in place;
`DELETE /api/flow/pairings/{device}` (`th flow pair revoke`) drops it and every
live bridge for it notices on its next frame. `GET /api/flow/pairings` never
serves `key_hex`.

The daemon's relay identity (`daemon-<12 hex>` from `~/.smooth/relay-device-id`
plus the hostname label) is resolved once at boot and shared by the relay
connection and the QR, so the link always names the daemon that will answer.

## Session kinds — harness manifests (th-0f6126)

A session's `kind` is `shell` or the `name` of a **harness manifest** — one
TOML file per coding agent CLI describing its binary, launch/resume argv,
state source, scrape patterns, steer and kill semantics
([Harness-Manifests.md](../Engineering/Harness-Manifests.md)). The engine's
launch table, binary resolver and pane scraper read manifests; the former
hard-coded table (th-5c5457) is now the built-ins, byte-for-byte:

| kind       | launch (binary + rendered `launch.argv`)             | harness session id                                        | restore (`flow.kill {resume:true}`, rule 2) | state                                                        |
| ---------- | ---------------------------------------------------- | --------------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------ |
| `claude`   | `claude --session-id <uuid> [--model m] [prompt]`    | pre-assigned by the engine                                | `claude --resume <uuid>`                    | `hooks` (smooth-agent plugin → `th flow hook`)               |
| `opencode` | `opencode [--model m] --prompt <prompt>` (th-b423aa) | learned from the first hook carrying the launch's token   | `opencode --session <id>`                   | `hooks` (smooth-agent OpenCode plugin posts the same body)   |
| `codex`    | `codex [--model m] <prompt>`                         | learned from the first tokened hook, when hooks are wired | `codex resume <id>`                         | `inferred` (pane scraping) until `~/.codex/hooks.json` posts |
| `th-code`  | `th code [--model m]`, prompt pasted ~4 s later      | pre-assigned; `SMOOTH_FLOW_SESSION` + `SMOOTH_URL` in env | relaunch (th code resumes by its own query) | `native` — th code POSTs `turn_start`/`turn_end` itself      |
| `shell`    | `$SHELL -l`                                          | —                                                         | never (shells don't resume)                 | `idle` from launch                                           |

Manifests load, lowest precedence first, from the built-ins,
`~/.smooth/harnesses/`, `<project>/.smooth/harnesses/`, and `th pkg`
packages' `harness/<name>/harness.toml`; `th harness list|show|add` manage
them. An unknown kind is refused at `flow.new` with the list to run.

Without a known harness session id, resume **relaunches the original argv**
— a fresh session, not a continuation. `Session.state_source` (`hooks` |
`native` | `inferred`) says how the engine knows the state; `th flow ls`
shows it in the VIA column.

**Binaries.** `which claude`/`codex` on a machine running cmux resolves to
cmux's CLI shims (`…/cmux-cli-shims/<uuid>/claude`), which inject cmux's own
`--session-id` and hooks. The manifest's `[binary]` therefore lists the real
installs first (`prefer_paths`: `~/.claude/local/claude`,
`~/.local/bin/claude`, `~/.opencode/bin/opencode`, `~/.local/bin/codex`),
then the first `PATH` hit not under a `skip_path_patterns` directory
(`cmux-cli-shims` by default), and the engine records the resolved path as
`argv[0]` in the session row. An explicit bare binary name in `flow.new.argv`
gets the same treatment. Codex 0.153+ also reads Claude-style hooks from
`~/.codex/hooks.json`; wiring `th flow hook` into it is
`th harness enable codex`'s job (pearl th-4ad334).

**Pane markers** are each manifest's `[state.scrape]` regexes: OpenCode
(`esc interrupt` = working, the `ctrl+p commands` status line without it =
idle) and Codex's menus (`› 1. …` + `press enter to confirm` = approval, e.g.
the trust-this-directory and hooks-need-review dialogs). A `usage_limit`
pattern may name a `reset` capture for the resume time.

### Harness list + prefs

`flow.hello` carries `harnesses: [{name, display_name, kind, installed,
binary_path, state_source, order_index, reason?, origin, health?}]` — the
pickers' list, in the user's order, hidden ones dropped — and `flow.harnesses
{harnesses}` is broadcast when it changes. `GET /api/flow/harnesses` returns
every manifest (hidden flagged); `PUT /api/flow/harnesses/prefs {order?,
hidden?}` persists `{order, hidden}` in flow.db's `config` table and
broadcasts. Every picker (macOS New-session sheet, fan-out candidates, the
phones) renders exactly this list: an uninstalled harness is disabled with
`reason`, never hidden, so the user learns what to install.

**Degraded harnesses (th-51bf88).** `health: {verdict, reason?, fix?}` is the
`th harness doctor` verdict (`works` | `degraded` | `not_installed`), computed
by the daemon itself (`smooth_flow::doctor`, the same core the CLI prints).
The daemon checks its OWN `PATH`, which is the app's when the app launched
it, so nothing is simulated. A pass runs each installed CLI's `--version`, so
it runs off-thread. The first pass starts with the first request for the list,
so the first `flow.hello` has no `health`. When verdicts change,
`flow.harnesses` is broadcast again, and a list requested more than two
minutes after the last pass starts a new one. A picker badges a degraded
harness ("needs setup"), shows `reason`, and offers `fix` to copy. The harness
stays startable, because degraded means part of a session will be missing
(hook-fed state, a login), not that it cannot run. SmoothFlow never runs the
fix. `SMOOTH_FLOW_HARNESS_DOCTOR=0` turns the doctor off (test rigs).

## Hooks — state comes from hooks, scraping is the fallback

`POST /api/flow/hooks` body `{harness, event, session_id, cwd, payload}`, with
the launch's hook token in `X-Smooth-Flow-Hook-Token`. The token names the
session; the body's `session_id` must match that row's `agent_session_id` (or,
for a row that has none yet, becomes it). The session's manifest decides the mapping: an
empty `state.hooks.event_map` means the Claude Code table below
(`protocol::map_hook_event`); a mapped harness (th code: `turn_start` →
working, `turn_end` → idle) uses its own names, and a `native` source marks
the row `native` instead of `hooks`.

| Event                                           | State                                                                                    |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `working`                                                                                |
| `Stop`                                          | `idle`, `unread = true`                                                                  |
| `PermissionRequest`                             | `needs_you` (`permission`), held open ≤120 s until `flow.approve`                        |
| `Notification` (permission / question / idle)   | `needs_you`                                                                              |
| `SessionEnd`                                    | nothing — the PTY decides done/dead (an ADOPTED row has no PTY, so this marks it `done`) |
| everything else                                 | nothing                                                                                  |

A `PermissionRequest` reply is the harness's own decision JSON
(`hookSpecificOutput.decision.behavior = allow|deny`; `allow_session` adds a
session-scoped `updatedPermissions` rule for the tool). Timing out replies `{}`
so the harness falls back to its own prompt — which the scraper then sees.

### `th flow hook` — the hook every harness runs (th-f97a27)

Every hook-capable harness is wired to the same native command, `th flow hook
<harness> <Event>`, which reads the hook payload on stdin and posts the
envelope above. It replaced the bash + curl + jq `flow-hook.sh`, which could
not run natively on Windows. The wire behavior is unchanged:

- **Discovery**: `$SMOOTH_FLOW_ADDR` → `~/.smooth/flow.addr` →
  `~/.smooth/daemon.addr` (see below). With no address it exits 0 and prints
  nothing.
- **Auth**: the token from `$SMOOTH_FLOW_HOOK_TOKEN_FILE` (hex only, ≤128
  chars) goes in `X-Smooth-Flow-Hook-Token`. Without a file the hook still
  posts, so an adopted session can report state, but it cannot be approved.
- **Envelope**: `session_id` is the first non-empty string of `.session_id`,
  `.sessionId` or `.conversation_id`. `cwd` is `.cwd`, then
  `.workspace_roots[0]`, then `$PWD`. `flow_id` comes from `$SMOOTH_FLOW_ID`.
  A payload that is not a JSON object is sent as `{"raw": "…"}`, and an empty
  one as `{}`.
- **Stdout**: gemini and copilot get `{}` printed first, and cursor-agent gets
  `{"continue":true}` (on `beforeSubmitPrompt`) or `{}`. For every other
  harness, `PermissionRequest` long-polls for up to 120 s
  (`FLOW_HOOK_PERMISSION_TIMEOUT`) and prints the reply verbatim, but only if
  it has a string at `.hookSpecificOutput.decision.behavior`. A `{}` reply, a
  4xx/5xx, a timeout or garbage prints nothing, and the harness asks the user.
  All other events are fire-and-forget with a 2 s budget (`FLOW_HOOK_TIMEOUT`).
- **Exit code**: always 0. `main` dispatches `flow hook` before clap, the auth
  profile setup and the log file, so a usage error can never become clap's
  exit 2, which would block a `PreToolUse`.

One difference from the script: the script detached its curl, while the
native hook waits for the loopback reply. Detaching is not portable, and a
second process would cost more than the POST. The 2 s budget still bounds the
wait, and events now arrive in order.

The overlays spell it `th flow hook <h> <Event> || exit 0`. The same string
works in sh, bash, cmd and PowerShell 7. When `th` is missing or predates
`flow hook`, the `|| exit 0` turns clap's exit 2 into a no-op instead of a
blocked tool call. Harnesses that parse stdout fall back to printing their
no-opinion answer instead (`|| echo '{}'`). Copilot entries also carry a
`powershell` form. `flow-hook.sh` remains for one release as a shim: it execs
`th flow hook` when the `th` on `PATH` has the command, and otherwise runs the
old curl path.

Scraping (`smooth_tmux::detect`, every 2 s on the visible pane) covers what
hooks can't: a usage limit ⇒ `limited` with `resume_at`; an approval menu with
no pending hook request ⇒ `needs_you`, answered by pressing the manifest's
`[steer] approve_keys` / `allow_session_keys` / `deny_keys` (th-5a2314:
`y` `Enter` for aider, `Enter` on goose's and crush's preselected Allow, `y`
for cline; Claude Code's `1` / `2` / `Escape` is the default); working/idle
only for sessions that have never reported a hook.

### Hook authentication (th-91d032)

The hooks endpoint is the one flow route the daemon's local token does not
gate, because its callers are shell scripts that third-party CLIs run. Left
open, any local process, web page (DNS rebinding) or tailnet peer could post
fake state, or put a forged permission prompt in front of the user. The fix
is a **per-launch hook token** (`smooth_flow::hook_auth`):

- Every time the engine launches an agent's process (`flow.new`, a resume, a
  crash relaunch), it mints 256 random bits. The token goes into
  `~/.smooth/flow-hook-tokens/<id>.token` (directory `0700`, file `0600`,
  replaced atomically). The pane gets only the file's path, as
  `SMOOTH_FLOW_HOOK_TOKEN_FILE`, so the secret is in neither argv (other users
  can read it on Linux) nor the environment (agents print that into
  transcripts). `flow.db` stores only its SHA-256, so both daemons that share
  the store can check it.
- `th flow hook` (and the legacy `flow-hook.sh`), the OpenCode plugin, `th code`
  and the fake-claude fixture read the file and send `X-Smooth-Flow-Hook-Token`.
  `th flow hook` writes the header on its own socket. `flow-hook.sh` handed it
  to curl as a config line on stdin. Neither puts it in an argument. Only hex
  survives the read, so a hostile file cannot inject a curl option or a header.
- The engine resolves the session **from the token**, never from the body. A
  hook whose `session_id` is not the row's is refused: a nested harness that
  inherited the pane's variable, or a token replayed against another
  session.
- A token dies with its launch. A relaunch rotates it, and `kill`, death and
  `close` revoke it (clearing the hash and deleting the file).

**Why per-session and not one daemon-wide token.** A daemon-wide secret in
every agent's environment would let any agent speak for every other
session. If it were the daemon's own token, it would also let any agent
spawn shells and approve prompts. A per-launch token grants exactly one
thing: reporting this session's hooks.

**Tokenless hooks** come from harnesses SmoothFlow did not launch:

| Target                                                                                     | Tokenless hook                                                                               |
| ------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------- |
| an engine-spawned row                                                                      | refused, even from loopback (its own hooks carry the token)                                  |
| an adopted row                                                                             | **state only**: working / idle / needs-you, events, `SessionEnd`                             |
| an adopted row's `PermissionRequest`                                                       | shown as `needs_you` with **no `request_id`**, answered `{}` at once; nothing can approve it |
| a new row (adoption, when on)                                                              | allowed, under the adoption guards below                                                     |
| anything, with `Origin`/`Sec-Fetch-Site`/`Forwarded`/`X-Forwarded-*`/`Tailscale-*` headers | refused: not a hook script talking to loopback                                               |

An adopted session gets no approvals because its identity is only a claim.
Anything that can reach the port can say it is that `claude`. An approvable
prompt would put text the claimant wrote next to that session's name, with an
Approve button, and would hold a long-poll open for whoever asked. The prompt
still shows, so the user knows to answer it in the terminal.

A presented but unknown token is refused outright and never falls back to
adoption: it is stale or forged. Every refusal is the same `200 {}` that an
unknown session gets, so a probe learns nothing, and the hook contract ("never
block the harness") holds.

**Out of scope:** code running as the same OS user. It can read the token files
and the daemon's `operator-token`. The boundary here is between sessions, and
between this user and everyone else: other users, browsers, tailnet peers, and
the kernel-sandboxed tool subprocesses, which cannot read `~/.smooth`.

**Upgrading.** An installed `smooth-agent` plugin whose `flow-hook.sh`
predates this change sends no token, so its hooks for SmoothFlow-launched
sessions are refused and those sessions fall back to scraped state.
`th harness doctor` flags this as degraded, with the fix: `th harness enable claude-code` (or `codex`/`opencode`), then restart the harness's sessions, because a running session keeps the hooks it started with. Codex also asks for its "Hooks need review" trust again.

## Zero friction — inference and adoption (th-c103c1)

Starting a session requires nothing but pressing Start. The New Session dialog
asks for a kind and (optionally) a prompt; everything else is **discovered**
from a directory and shown read-only, with a disclosure for explicit
overrides. Start is never blocked on a missing pearl.

### What is inferred (`smooth_flow::infer`)

`GET /api/flow/infer?cwd=…` (and `th flow infer`) answers, for one directory:

| Field      | Resolution                                                                                                                                                                 |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `worktree` | `git rev-parse --show-toplevel`, else the cwd itself                                                                                                                       |
| `project`  | `--git-common-dir`'s parent — the MAIN checkout even inside a linked worktree (the pearls rule)                                                                            |
| `branch`   | `--abbrev-ref HEAD`; `None` when detached (`HEAD`) or unborn                                                                                                               |
| `pearl_id` | the pearl store's answer for this worktree, else `th-xxxxxx` at the front of the branch, else the same run anywhere in the worktree directory name (`smooth-th-c103c1-zf`) |
| `jira_key` | `SMOODEV-1234` (uppercase only) in the branch, else the worktree name, else the pearl's title/description                                                                  |
| `title`    | the pearl's title, else the branch (`main`/`master`/`trunk`/`develop` say nothing and are skipped), else the directory name                                                |

`infer::infer` is pure over explicit facts and exhaustively tested;
`infer::gather` is the thin shell that runs `git` and `th pearls`. Every field
is independently optional: a non-git directory, a detached HEAD, a bare repo
and a worktree whose pearl was deleted all infer what they can and drop the
rest. A pearl id parsed off the branch survives a store miss — the worktree is
still that pearl's worktree, the title is just unknown.

`flow.new` runs the same inference on the resolved worktree, so any client
(the phone, `th flow new`, the dialog) gets the pearl, branch and title
without sending them. An explicit value always wins. `th flow new` with no
arguments starts a session in the CURRENT directory.

### Where the session runs — the repo index (th-145e6b)

The New Session sheet's **Directory** field searches every git checkout
under `$HOME`. The field starts on the inferred worktree. You can type to
search, type or paste a path (`~/…` or `/…`), or use **Browse…** to open a
folder panel. Picking a directory re-runs inference for it, so the pearl,
branch and title follow.

`smooth_flow::repos` walks `$HOME` with the `ignore` crate's parallel
walker, the engine inside `fd`. It stops at each repo root and never
descends into:

- hidden directories;
- dependency and build trees (`node_modules`, `target`, `vendor`, …);
- top-level `~/Library`, media folders, and cloud-synced folders (Google
  Drive, iCloud, OneDrive, Dropbox), where walking would make macOS download
  files.

A linked worktree (`.git` is a file) is listed as its own row, with the main
checkout it belongs to. The branch is read from `HEAD`; `git` is never run.
Rows live in flow.db's `repos` table. The daemon scans in the background at
start and again when a query finds the last scan more than 10 minutes old, so
a restarted daemon answers from the last scan at once.

`GET /api/flow/repos?q=&limit=` returns `{repos: [{path, name, branch?,
main?, touched}], scanning, indexed}`. Every whitespace-separated token must
match, case-insensitively. Rows rank by exact name, then name prefix, name
substring, path substring, branch, and finally a fuzzy match on the path.
Checkouts the fleet already works in rank first, then the most recently
touched. `POST /api/flow/repos/rescan` re-walks now, for a repo cloned a
minute ago. `EngineConfig::repo_root` is `None` by default, so tests and
scratch engines never walk a real home.

### Adoption — plain `claude` / `codex` sessions join the fleet

The hook overlay `th pkg` installs into every harness already posts
`{harness, event, session_id, cwd}` on every lifecycle event — including from
a `claude` someone started in an ordinary terminal. When a hook names a
session the engine has no row for, it can **adopt** it: infer the context from
`cwd` and create a row, so that session appears in the fleet with its pearl,
branch and worktree attached.

It is **off by default** — adoption puts rows in the fleet the user never
asked for. `th flow adopt on` (or `PUT /api/flow/settings`, or
`SMOOTH_FLOW_ADOPT=1`) turns it on.

Guards, all of them deliberate (`engine::adoptable`, each with its own refusal
reason):

| Guard                             | Why                                                                                                                   |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| opt-in required                   | the fleet is the user's, not the hook's                                                                               |
| a harness session id              | there is nothing to key a row on without one                                                                          |
| never a `PermissionRequest` first | adopting there would hold a plain terminal open ≤120 s for a decision nobody is watching yet                          |
| a known harness                   | `harness` must name a manifest (`claude-code` → the `claude` manifest)                                                |
| a git worktree                    | SmoothFlow tracks work on branches; a shell in `/tmp` is not fleet work                                               |
| a known project                   | the daemon's workspace, or a project some session row already lives in — otherwise every repo on the machine leaks in |

A refusal is cached per harness session id, so the git/`th` shell-outs happen
once rather than on every tool call. The check-and-create runs under the store
lock, so two concurrent hooks from one session cannot make two rows.

**What an adopted session can and cannot do.** SmoothFlow did not spawn its
PTY, so:

- ✅ it appears in the fleet with title, pearl, Jira key, branch, worktree;
  its state tracks its hooks (working / idle / needs-you), its events stream
  into the timeline. Its permission requests show as needs-you, but are
  answered in its own terminal, not from SmoothFlow (th-91d032: its hooks
  carry no token, so its identity is only a claim).
- ❌ **no attach** — there is no tmux pane; drive it in the terminal it is
  running in.
- ❌ **no kill, no resume** — the engine does not own that process, and
  supervision skips adopted rows entirely.
- ⚠️ **lifecycle by hook only** — its own `SessionEnd` marks it `done`; a
  terminal closed without one leaves it live until the supervision tick calls
  it `dead` after 6 hours of silence.
- ⚠️ **close refuses while it is live** (nothing here can stop it, and
  removing its worktree would be destructive); `--force` overrides.

### `~/.smooth/flow.addr` — how hooks find the flow engine

`flow-hook.sh` used to discover the daemon through `~/.smooth/daemon.addr`
only. Since PR #546 the SmoothFlow app's child daemon deliberately does **not**
write that file (th-3e6b1b: a second instance repointing `th`, the hooks and
Big Smooth's clients at itself is the bug that file fixed) — so on a machine
where SmoothFlow is the only daemon, hooks had nowhere to post and adoption
could never fire.

`~/.smooth/flow.addr` is the second file, owned by the flow engine rather than
by the daemon identity. Both daemons host a flow engine and both share one
`~/.smooth/flow.db`, so either can service a hook correctly; what matters is
that some LIVE flow engine is reachable. The claim rule (`flow_addr`):

- no file, or one naming an address that no longer answers `/health` → claim it;
- a file naming a live daemon → leave it (that daemon serves the hooks);
- our own address → rewrite it (a restart on the same port);
- the SmoothFlow app's engine (`$SMOOTHFLOW_PARENT` set) takes it even from a
  live holder (th-5069eb). The app launches its daemon with its own
  `smoothflow-flow.db` and tmux socket, so the stores are **not** shared in
  practice: a long-running Big Smooth holding the file sent hooks, `th flow`,
  the MCP flow tools and SmoothFlow Desktop to an engine that had never heard
  of the app's sessions;
- released on shutdown (Ctrl-C or SIGTERM, which is how the app stops its
  daemon), and only when it is still ours.

Discovery chain, in `th flow hook` and the OpenCode plugin alike:
`$SMOOTH_FLOW_ADDR` → `~/.smooth/flow.addr` → `~/.smooth/daemon.addr`. Nothing
changes on a machine that runs only Big Smooth.

**Two daemons, one store.** Because they share `flow.db` but not a broadcast
channel, a hook that lands on the other daemon is invisible to this one's
clients until something re-reads the store. The supervision tick therefore
re-broadcasts rows it did not write (`rebroadcast_external_changes`, keyed on
`updated_at`); a `flow.session` frame is an upsert, so a re-emit is harmless.
The remaining seam is a couple of seconds of latency on the non-owning
daemon's clients — and, in a millisecond-wide race, two daemons adopting the
same brand-new harness session into two rows.

## Supervision rules

1. Pre-assign `--session-id` for claude; store argv, cwd, tmux session, pid +
   start time.
2. Unexpected death (non-zero exit, or the tmux session gone while state ≠
   done) ⇒ relaunch with `claude --resume <id>` up to 3 times with 5 s · 2ⁿ
   backoff, then `dead` with attention `crashed`. Shells never resume.
3. Usage limit ⇒ schedule, not give up: parse the reset time
   (`limit::parse_reset_at` — "resets at 4pm", "in 45 minutes"; fallback 1 h),
   set `limited` + `resume_at`, and when the tick passes it send Enter (or
   `--resume` if the pane died). The stale banner is ignored for 90 s after.
4. Duplicate-resume guard: a 60 s claim on the agent session id plus pid
   liveness, and any other live row owning the same id. A held id raises
   attention `held` with the holder pid instead of launching.
5. Exit code 0 is unproven unless the PTY (`#{pane_dead_status}`) or the pane
   wrapper's exit file reported it. An exit nobody can read is **unknown**:
   `dead`, attention `crashed` with "exit status unknown", and never resumed.
6. Engine, tmux server and agents are spawned by the app (or its LaunchAgent)
   so TCC grants attribute to it. `th flow` connects; it never launches the
   daemon.

## Fan-out

`flow.fanout.new {prompt, pearl_id, candidates:[{kind, model, label}]}` records
`base_commit = HEAD` of the project, then per candidate: `git worktree add
../<repo>-<pearl>-<label> -b <pearl>-<label> <base>`, `th pearls create` a child
pearl (label `fanout`, run from the main checkout so it isn't lost with a
worktree), and a session titled `<pearl> · <label>` with the prompt. Every
candidate carries the `fan_out_id`.

`flow.fanout.pick {fan_out_id, winner_session_id}` runs the `th worktree merge`
steps in the main checkout (`checkout main`, `pull --rebase`, `merge <branch>
--no-ff`), then for each loser: kill the session, `git worktree remove
--force`, `git branch -D`. Child pearls of every candidate are closed with one
`th pearls close`. Session rows (transcripts) are kept.

## Pearl rail

`GET /api/flow/sessions/{id}/handoff` (and `flow.handoff` over WS) returns
`{pearl, handoff:{worktree, branch, head, dirty, agent_session_id, next},
checkpoints:[{at, note, auto}], blocks:[ids], pr:{number, url, ci}|null}`. Git
facts come from the engine; `pearl`/`checkpoints`/`blocks`/`next` come from
`th pearls show <id> --handoff --json` (lane C, th-9483e8 — the packet has
exactly this shape) when the installed `th` has it, else `pearl` degrades to
`{id, text}` from the plain `th pearls show` and the lists to `[]`; `pr`
comes from `gh pr list --head <branch>` (falling back to the packet's) and is
`null` without `gh`.

## Big Smooth drives the fleet {#big-smooth-drives-the-fleet}

> th-8b3918 (epic Phase 2). The tool foundation for an in-app copilot that sets
> up projects, starts and steers coding agents, and watches the fleet.

Big Smooth's own agent (the operator `LocalServer` the daemon hosts) gets the
SmoothFlow verbs as ordinary tools on its per-turn registry. They call the flow
`Engine` **in-process**: no HTTP, no token. `serve_local_flavor` opens the
engine (`flow_route::install`) _before_ it builds the tool provider and hands it
in (`local_tool_provider_with_flow(…, Some(engine))`). The ephemeral and test
providers pass `None` and get no flow tools.

The names, argument schemas and answers match the MCP tools in
`smooth-cli/src/mcp_flow.rs`, so a model sees one vocabulary through either
door. The rules both share live in `smooth_flow::vocab`: the list filter, the
one-line session summary, the prompt refusal, and `turn_progress`
(settled / stalled / running).

| Tool                                                                                       | Class       | Gate                                  |
| ------------------------------------------------------------------------------------------ | ----------- | ------------------------------------- |
| `flow_list`, `flow_snapshot`, `flow_handoff`, `flow_harnesses`, `flow_repos`, `flow_infer` | read        | none; kept in Plan mode               |
| `flow_new`, `flow_send`, `flow_prompt_wait`, `flow_fanout_new`, `project_setup`            | write       | confirm; dropped in Plan mode         |
| `flow_kill`, `flow_close`, `flow_fanout_pick`                                              | destructive | confirm; dropped in Plan mode         |
| `flow_approve`                                                                             | approve     | confirm, always; dropped in Plan mode |

- **Confirm** is the daemon's `CONFIRM_TOOLS` floor (`operator.rs`). Core's
  `ConfirmationHook` parks the turn on `write_confirmation_required` until the
  user answers. The floor is merged in whatever `SMOOTH_AGENT_CONFIRM_TOOLS`
  says, and `SMOOTH_AUTO_MODE` never reads it, so `bypass` doesn't skip it.
  `flow_approve` answers another agent's permission prompt, which is the
  user's call by definition.
- **Plan mode** keeps the six reads, via `PLAN_READONLY_TOOLS`, and drops
  everything else.
- **Demo mode** (`SMOOTH_DEMO`) drops all fifteen, because a reviewer must not
  see the host's sessions. Family roles get them only when a role grants them.
- **Sidekicks.** A `send_sidekick` snapshot never includes a confirm-gated
  tool. Every sidekick call runs the daemon's host hooks (the permission gate,
  then Narc; th-8d1951), but the per-turn confirmation gate is not one of them,
  so such a tool would run unconfirmed there.
- **Hooks.** The permission gate and Narc see every flow call like any other
  tool.
- **No sandbox.** A flow session runs on the host, outside the kernel sandbox
  that confines `bash`: an agent session is a process on this machine.
  `project_setup`'s `git clone` also runs on the host. That is why every write
  parks for the user.

`flow_prompt_wait` refuses a blocked agent (`needs_you`, `limited`, `done`,
`dead`) without typing anything. Otherwise it sends the prompt and follows
the engine's `flow.session` broadcast, so a brief `working` between two polls
still counts. A 2-second re-read is the backstop. The wait ends settled, or
stalled (the agent never started working within 60s), or at its timeout
(default 600s, max 3600s).

`flow_close` never passes `force`. A dirty or unmerged worktree is reported
back, and forcing past it is the user's call, made in the app or the CLI.

### `project_setup`

`project_setup {repo, clone_into?, pearl_id?, branch?, kind?, prompt?, title?, model?}`
does in one confirmed call what a person does in the New Session dialog:

1. **The checkout.** `repo` is either a local path or something to clone.
    - A local path can be absolute, `~/…`, or relative to the turn's cwd. It is
      used as it is.
    - A git URL is cloned to `<clone_into>/<name>`. Accepted URLs are
      `https://`, `http://`, `ssh://`, `git://`, `file://` and scp-style
      `git@host:owner/repo`; anything else, `ext::` included, is treated as a
      path. A local **bare** repo path is also cloned.
    - `clone_into` defaults to `~/dev` under the engine's `$HOME`. There is no
      new env var for it. A checkout already at the destination is reused when
      its `origin` matches, and refused otherwise.
2. **Where the agent runs.**
    - With `branch`: `Engine::create_branch_worktree` makes
      `../<repo>-<slug(branch)>` on exactly that branch. It checks out the
      branch if it exists, else creates it from `HEAD`. Git validates the name,
      and a name starting with a dash is refused.
    - With only `pearl_id`: the engine's own pearl worktree
      (`../<repo>-<pearl>-<slug>`), the same path as `flow.new` with a pearl.
    - With neither: the checkout itself.
3. **The session.** It starts `kind` (default `claude`), with the first prompt
   when one is given. The answer names the checkout, the worktree, the branch
   and the session id. It also reminds the model that a prompted agent is
   already working, so it should not be prompted again.

Tests: `crates/smooth-daemon/src/flow_tools/tests.rs` drives every tool against
a real engine on a scratch `$HOME`, with sessions in smooth-flow's in-memory
`FakeHost` (the `test-util` feature). It covers list/new/send, `prompt_wait`
settling, stalling and refusing, approve, kill, close, and `project_setup`
against a local bare repo. `operator.rs` pins the permission classes: the
confirm floor, the Plan allowlist, demo exclusion, and the provider wiring.

## Testing

`cargo test -p smooai-smooth-flow` covers the store, the state machine, the
frame round-trips, the hook table, the reset-time parser, the guard, the PTY
bridge and — when `tmux` is on `PATH` — a live shell session end to end
(launch, stream, send, snapshot, kill, death detection). `smooth-daemon`'s
`flow_route` tests drive the WS + the hook long-poll over a real socket;
`relay` tests pin the channel routing, the phone caps and the end-to-end
guard (`FlowGuard`: pair → open → data, plaintext from a paired phone refused,
revoke mid-session); `flow_e2e` tests pin the primitives (an RFC 7748 vector,
nonce layout, tamper + replay rejection) and regenerate/verify the shared
fixture (`SMOOTH_E2E_WRITE_FIXTURE=1` rewrites it). All live tests name a
private tmux socket per call (`tmux_socket` on the request) so they never
touch a running daemon's sessions.

End to end (th-8e3087): `crates/smooth-daemon/tests/flow_e2e` boots a REAL
`smooth-daemon` per test — its own HOME, port, tmux server — and drives it the
way the apps, `th flow` and a harness's hook script do, with `fake-agent`
installed through a manifest in the four state-source flavours. The full
strategy (what runs where, the fake-agent contract, runtimes, the CI split) is
[SmoothFlow-Testing.md](../Engineering/SmoothFlow-Testing.md); the macOS UI
lane is [SmoothFlow-Testing-macOS.md](../Engineering/SmoothFlow-Testing-macOS.md).
