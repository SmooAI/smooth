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

| Piece                                                    | Path                                                        |
| -------------------------------------------------------- | ----------------------------------------------------------- |
| Engine crate (store, tmux glue, PTY, supervision)        | `crates/smooth-flow/`                                       |
| Daemon transport (`/api/flow/*`, WS, hooks long-poll)    | `crates/smooth-daemon/src/flow_route.rs`                    |
| Relay routing of `channel:"flow"` envelopes + phone caps | `crates/smooth-daemon/src/relay.rs`                         |
| Shared pane-state heuristics (moved from `th claude`)    | `crates/smooth-tmux/src/detect.rs`                          |
| CLI                                                      | `crates/smooth-cli/src/flow.rs` (`th flow …`)               |
| Session store                                            | `~/.smooth/flow.db` (SQLite, WAL; `$SMOOTH_FLOW_DB`)        |
| tmux server                                              | `tmux -L smooth-flow` — see [tmux socket](#tmux-socket-tcc) |

## Process model

```
Big Smooth.app / th up ──► smooth-daemon ──► smooth_flow::Engine
                                              │
                                              ├── tmux -L smooth-flow   (outlives the daemon)
                                              │     ├── fs-1a2b3c4d: sh -c 'exec claude --session-id <uuid> …'
                                              │     └── fs-9e8f7a6b: sh -c 'exec zsh -l'
                                              │
                                              └── per attached session: portable-pty ⟷ `tmux attach -t fs-…`
                                                                          │
      th flow attach / macOS app / phone  ◄── GET /api/flow/ws ◄──────────┘  flow.output {data_b64}
```

- **Sessions run under tmux, not under the daemon.** A daemon restart or an
  app crash never kills a PTY: the engine re-opens `flow.db`, finds the tmux
  session by name (the flow session id) and carries on. `remain-on-exit` is on
  so a dead pane stays until the engine has read `#{pane_dead_status}` — the
  PTY's own exit report, which is the only proof of exit 0 the spec accepts
  (rule 5).
- **`exec` in the pane.** The launch line is `sh -c 'exec <argv>'`, so the pane
  pid _is_ the agent's pid. The engine records `pid` + start time (from
  `ps -o lstart=`) as the liveness index: a recycled pid can't pass for the agent.
- **Streaming is a tmux client on a PTY, not `pipe-pane`.** `pipe-pane` yields
  bytes positioned for the pane's own geometry and only from the moment the
  pipe opens, so a late-joining client gets no initial screen. A `tmux attach`
  client on a `portable-pty` gets a full redraw on attach, follows the PTY's
  size (`window-size latest`), and passes keystrokes through untouched —
  lossless bytes + resize, which is what a terminal-emulator surface needs. The
  PTY exists only while ≥1 client is attached; the last `flow.detach` drops it
  and the tmux session keeps running detached.

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
  carry the token for the operator WS. Hooks stay open on purpose: the hook
  script must never block the harness, and it can only touch a session whose
  pre-assigned 128-bit id it already knows.
- **Relay (phones).** The envelope stays `{to, frame}`. A frame carrying
  `"channel":"flow"` is bridged to `/api/flow/ws` by a second per-phone
  loopback bridge; no `channel` ⇒ operator WS, unchanged. Outbound
  `flow.output` to a phone is coalesced to ~30 fps and split into ≤16 KiB
  frames (`relay::OutputCoalescer`); phones never receive raw scrollback.
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
`flow.fanout.new`, `flow.fanout.pick`, `flow.mark_read`, and (v0.1)
`flow.hello`, `flow.handoff`.

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

### tmux socket (TCC) {#tmux-socket-tcc}

Sessions are created on the tmux server named by, in order: the
`tmux_socket` field of `flow.new` (`th flow new --tmux-socket <name>`), then
`smooth-daemon operator --tmux-socket <name>` / `$SMOOTH_FLOW_TMUX_SOCKET`,
then the default `smooth-flow`. Every row records its socket, so a daemon
restarted with a different setting still finds its old panes.

**Why it matters:** on macOS, TCC attributes a pane's grants (Full Disk
Access, Calendar, Notifications) to the process that started the tmux
_server_. The SmoothFlow app starts `tmux -L smoothflow` itself (as a direct
child, on every launch) and passes `SMOOTH_FLOW_TMUX_SOCKET=smoothflow` to
the daemon it spawns. A session created on any other socket — the default
`smooth-flow` server, or one a terminal started — runs agents with **no**
FDA/Calendar access, and the failures are silent (empty listings, "not
authorized" from EventKit), not prompts. `th flow new` from a terminal
therefore lands on the app's server only with `--tmux-socket smoothflow`.

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
| `opencode` | `opencode [--model m] --prompt <prompt>` (th-b423aa) | learned from the plugin's `session.created` by cwd        | `opencode --session <id>`                   | `hooks` (smooth-agent OpenCode plugin posts the same body)   |
| `codex`    | `codex [--model m] <prompt>`                         | learned from a hook by cwd when hooks are wired           | `codex resume <id>`                         | `inferred` (pane scraping) until `~/.codex/hooks.json` posts |
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

`POST /api/flow/hooks` body `{harness, event, session_id, cwd, payload}`. The
engine matches `session_id` to `sessions.agent_session_id` — that is why
session ids are pre-assigned. The session's manifest decides the mapping: an
empty `state.hooks.event_map` means the Claude Code table below
(`protocol::map_hook_event`); a mapped harness (th code: `turn_start` →
working, `turn_end` → idle) uses its own names, and a `native` source marks
the row `native` instead of `hooks`.

| Event                                           | State                                                             |
| ----------------------------------------------- | ----------------------------------------------------------------- |
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `working`                                                         |
| `Stop`                                          | `idle`, `unread = true`                                           |
| `PermissionRequest`                             | `needs_you` (`permission`), held open ≤120 s until `flow.approve` |
| `Notification` (permission / question / idle)   | `needs_you`                                                       |
| `SessionEnd`                                    | nothing — the PTY decides done/dead                               |
| everything else                                 | nothing                                                           |

A `PermissionRequest` reply is the harness's own decision JSON
(`hookSpecificOutput.decision.behavior = allow|deny`; `allow_session` adds a
session-scoped `updatedPermissions` rule for the tool). Timing out replies `{}`
so the harness falls back to its own prompt — which the scraper then sees.

Scraping (`smooth_tmux::detect`, every 2 s on the visible pane) covers what
hooks can't: a usage limit ⇒ `limited` with `resume_at`; an approval menu with
no pending hook request ⇒ `needs_you` (answered by keystroke: `1` / `2` /
`Escape`); working/idle only for sessions that have never reported a hook.

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
5. Exit code 0 is unproven unless the PTY reported it (`#{pane_dead_status}`).
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
`relay` tests pin the channel routing and the phone caps. All live tests name a
private tmux socket per call (`tmux_socket` on the request) so they never
touch a running daemon's sessions.
