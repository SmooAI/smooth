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

## Where the pieces live

| Piece                                                    | Path                                                         |
| -------------------------------------------------------- | ------------------------------------------------------------ |
| Engine crate (store, tmux glue, PTY, supervision)        | `crates/smooth-flow/`                                        |
| Daemon transport (`/api/flow/*`, WS, hooks long-poll)    | `crates/smooth-daemon/src/flow_route.rs`                     |
| Relay routing of `channel:"flow"` envelopes + phone caps | `crates/smooth-daemon/src/relay.rs`                          |
| End-to-end encryption + phone pairing (th-d98fde)        | `crates/smooth-daemon/src/flow_e2e.rs`, `flow_pair_route.rs` |
| Shared pane-state heuristics (moved from `th claude`)    | `crates/smooth-tmux/src/detect.rs`                           |
| CLI                                                      | `crates/smooth-cli/src/flow.rs` (`th flow …`)                |
| Session store                                            | `~/.smooth/flow.db` (SQLite, WAL; `$SMOOTH_FLOW_DB`)         |
| tmux server                                              | `tmux -L smooth-flow` — see [tmux socket](#tmux-socket-tcc)  |

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

Engine → clients (broadcast; `flow.output` only to clients that attached
the id): `flow.hello`, `flow.session`, `flow.session.removed`, `flow.output`,
`flow.screen`, `flow.attention`, `flow.fanout`, `flow.error`, (v0.1)
`flow.event`, `flow.handoff`, and (th-0f6126) `flow.harnesses`.

Clients → engine: `flow.attach`, `flow.detach`, `flow.input`, `flow.resize`,
`flow.snapshot`, `flow.new`, `flow.send`, `flow.approve`, `flow.kill`,
`flow.fanout.new`, `flow.fanout.pick`, `flow.mark_read`, (v0.1)
`flow.hello`, `flow.handoff`, and (th-e126cc) `flow.close`.

Field-level shapes are the types in `crates/smooth-flow/src/protocol.rs`
(`ClientFrame`, `ServerFrame`), which the round-trip tests pin. One additive
field: `flow.fanout.new` accepts `project` (the main checkout to fan out from;
defaults to the daemon's workspace).

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
| `claude`   | `claude --session-id <uuid> [--model m] [prompt]`    | pre-assigned by the engine                                | `claude --resume <uuid>`                    | `hooks` (smooth-agent plugin → `flow-hook.sh`)               |
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
`~/.codex/hooks.json`; wiring `flow-hook.sh` into it is
`th harness enable codex`'s job (pearl th-4ad334).

**Pane markers** are each manifest's `[state.scrape]` regexes: OpenCode
(`esc interrupt` = working, the `ctrl+p commands` status line without it =
idle) and Codex's menus (`› 1. …` + `press enter to confirm` = approval, e.g.
the trust-this-directory and hooks-need-review dialogs). A `usage_limit`
pattern may name a `reset` capture for the resume time.

### Harness list + prefs

`flow.hello` carries `harnesses: [{name, display_name, kind, installed,
binary_path, state_source, order_index, reason?, origin}]` — the pickers'
list, in the user's order, hidden ones dropped — and `flow.harnesses
{harnesses}` is broadcast when it changes. `GET /api/flow/harnesses` returns
every manifest (hidden flagged); `PUT /api/flow/harnesses/prefs {order?,
hidden?}` persists `{order, hidden}` in flow.db's `config` table and
broadcasts. Every picker (macOS New-session sheet, fan-out candidates, the
phones) renders exactly this list: an uninstalled harness is disabled with
`reason`, never hidden, so the user learns what to install.

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
- `flow-hook.sh`, the OpenCode plugin, `th code` and the fake-claude fixture
  read the file and send `X-Smooth-Flow-Hook-Token`. `flow-hook.sh` hands the
  header to curl as a config line on stdin, never as an argument. Only hex
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
- released on shutdown, and only when it is still ours.

Discovery chain, in `flow-hook.sh` and the OpenCode plugin alike:
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
