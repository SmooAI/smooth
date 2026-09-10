# SmoothFlow flow protocol — v0 contract (shared by every lane)

Epic th-6ac036. Wireframes: https://claude.ai/code/artifact/f5c898f7-c2eb-4017-8d9e-a8f54c3c5c95

The engine (`smooth-flow` crate, hosted in smooth-daemon) is the ONLY state holder. Shells (macOS app, `th flow`, phone) are dumb views. Change this file only by agreement of the engine lane; other lanes code against it and mock it.

## Transport

- **Own WebSocket**, not the operator's canonical WS: `GET /api/flow/ws` on the daemon (axum upgrade, mounted via `serve_routes` in `crates/smooth-daemon/src/operator.rs`). The daemon cannot intercept the engine's WS, so flow is a sibling channel. Same loopback origin, same `~/.smooth/daemon.addr` discovery.
- **HTTP** siblings for one-shot calls: `GET /api/flow/sessions`, `POST /api/flow/sessions`, `POST /api/flow/sessions/{id}/input`, `POST /api/flow/sessions/{id}/resize`, `POST /api/flow/sessions/{id}/approve`, `POST /api/flow/sessions/{id}/kill`, `POST /api/flow/hooks` (see Hooks), and the phone-pairing set (th-d98fde): `POST /api/flow/pair` (mint a QR → `{pairing_id, url, code, device, label, expires_at, relay_enabled}`), `GET /api/flow/pair/{id}` (`{state: pending|paired|expired|unknown, …}`), `GET /api/flow/pairings`, `DELETE /api/flow/pairings/{device}` — the mock answers all four (a scan "happens" on the third poll).
- **Relay** (phones): the relay envelope stays `{to, frame}`. A frame carrying `"channel":"flow"` is routed by the daemon's relay bridge (`crates/smooth-daemon/src/relay.rs`) to the flow WS handler instead of the operator loopback WS. No `channel` ⇒ operator WS (unchanged, backward compatible). Phones NEVER receive raw scrollback: only `flow.output` deltas capped at 16 KiB per frame and `flow.screen` snapshots on demand.
- Every frame is one JSON object: `{"channel":"flow","type":"<name>", ...fields}`. Unknown types are ignored, never fatal.

## Session model (SQLite, `~/.smooth/flow.db`, WAL)

```
sessions(
  id TEXT PK,             -- "fs-" + 8 hex
  kind TEXT,              -- "shell" | "claude" | "codex" | "opencode"
  title TEXT,
  project TEXT,           -- main repo root (same resolution as pearls: git-common-dir parent)
  worktree TEXT,          -- absolute path the PTY runs in
  branch TEXT,
  pearl_id TEXT NULL,     -- th-xxxxxx
  agent_session_id TEXT NULL, -- pre-assigned `claude --session-id` uuid / codex id
  argv TEXT,              -- JSON array actually launched
  tmux_session TEXT NULL, -- smooth-tmux session name (agents run under tmux; shells too)
  pid INTEGER NULL, pid_start INTEGER NULL, -- liveness index (pid + start time)
  state TEXT,             -- "starting"|"working"|"idle"|"needs_you"|"limited"|"done"|"dead"
  attention TEXT NULL,    -- JSON: {"reason":"permission"|"question"|"usage_limit"|"crashed"|"held", "detail":..., "resume_at": rfc3339|null, "request_id":...}
  fan_out_id TEXT NULL,   -- groups candidates of one fan-out
  created_at TEXT, updated_at TEXT, ended_at TEXT NULL, exit_code INTEGER NULL
)
fan_outs(id TEXT PK, prompt TEXT, base_commit TEXT, pearl_id TEXT, created_at TEXT, winner_session_id TEXT NULL)
```

Timestamps: UTC RFC3339 text, compared with Rust `Utc::now()` literals, never SQLite `now`.

## Frames, engine → clients (broadcast to every flow WS)

- `flow.hello` `{daemon:{version, machine_label}, sessions:[Session]}` — on connect; `Session` = the row above minus pid_start, plus `unread:bool`.
- `flow.session` `{session: Session}` — any change (state, attention, title, branch, pearl).
- `flow.session.removed` `{id}`
- `flow.output` `{id, seq:u64, data_b64}` — raw PTY bytes; **only sent to clients that `flow.attach`ed** that id. Desktop feeds it straight into the ghostty surface. Phones get it only while attached, throttled to 30 fps and ≤16 KiB/frame.
- `flow.screen` `{id, cols, rows, text}` — plain-text snapshot of the visible pane (from tmux capture), reply to `flow.snapshot`. This is what the phone renders when not streaming.
- `flow.attention` `{id, attention}` — emitted in addition to `flow.session` so notifications can key on it.
- `flow.fanout` `{fan_out: FanOut, candidates:[Session]}`
- `flow.error` `{ref:<client seq>|null, code, message}` — error objects, never bare strings.

## Frames, client → engine

- `flow.attach` `{id, cols, rows}` / `flow.detach` `{id}` — subscribe to output; attach also sets the PTY size.
- `flow.input` `{id, data_b64}`
- `flow.resize` `{id, cols, rows}`
- `flow.snapshot` `{id}`
- `flow.new` `{kind, worktree|null, project|null, pearl_id|null, prompt|null, argv|null, title|null}` → reply `flow.session`. For `kind:"claude"` with no argv the engine launches `claude --session-id <uuid> [prompt]` under smooth-tmux in the worktree (creating the worktree via `th worktree create` when `pearl_id` is set and `worktree` is null: `../<repo>-<pearl>-<slug>`).
- `flow.send` `{id, text}` — steer: text + Enter into the agent's prompt (tmux bracketed paste), NOT raw bytes.
- `flow.approve` `{id, request_id, decision:"allow"|"deny"|"allow_session"}` — answers a permission attention. For hook-reported permission requests the engine replies to the hook's pending HTTP request; for scraped ones it sends the keystroke.
- `flow.kill` `{id, resume:bool}` — kill the pid tree; if `resume`, relaunch with `claude --resume <agent_session_id>` (duplicate-resume guard: refuse if another live pid owns that session id).
- `flow.fanout.new` `{prompt, pearl_id, candidates:[{kind, model|null, label}]}` → creates N worktrees + N sessions + N child pearls.
- `flow.fanout.pick` `{fan_out_id, winner_session_id}` → `th worktree merge` the winner, GC the losers' worktrees, close their child pearls, keep transcripts.
- `flow.mark_read` `{id}`

## Hooks (state comes from hooks, scraping is the fallback)

`POST /api/flow/hooks` body `{harness:"claude-code", event, session_id, cwd, payload}` where `event` ∈ `SessionStart|UserPromptSubmit|PreToolUse|PostToolUse|PermissionRequest|Notification|Stop|SubagentStop|PreCompact|SessionEnd` and `payload` is the harness's hook JSON verbatim. The engine matches `session_id` to `sessions.agent_session_id` (that is why session ids are pre-assigned). `PermissionRequest` is held open (long-poll, 120 s max) until `flow.approve` answers; reply body is the harness's expected decision JSON. Every other event replies `200 {}` immediately. Hook scripts must exit 0 and stay silent when the daemon is unreachable (never block the harness).

State mapping: `UserPromptSubmit|PreToolUse|PostToolUse` ⇒ working; `Stop` ⇒ idle (unread=true); `PermissionRequest|Notification(permission|question)` ⇒ needs_you; `SessionEnd` ⇒ done or dead by exit; usage-limit text on the pane ("resets at …") ⇒ limited with `resume_at`, and the engine sleeps until then and sends Enter/`--resume`.

## Supervision rules (engine)

1. Pre-assign `--session-id` for claude; store argv, cwd, tmux session, pid + start time.
2. On unexpected death (non-zero exit, or pid gone while state≠done): relaunch with `--resume` up to 3 times, exponential backoff, then state=dead with attention reason crashed.
3. Usage limit ⇒ schedule, not give up: parse the reset time (`detect.rs` already matches the phrase), `resume_at`, sleep_interruptible, resume.
4. Duplicate resume guard (60 s claim + pid liveness) — refuse and raise attention reason `held` with the holder pid.
5. Exit code 0 is unproven unless the PTY reported it.
6. Engine, tmux server, and agents are spawned by the macOS app (or a LaunchAgent inside its bundle) so TCC grants (Calendar, Full Disk Access, Notifications, Apple Events) attribute to the app. `th flow` connects; it never launches the daemon.

## `th flow` CLI (thin client, in smooth-cli)

`th flow ls|new|attach|send|approve|kill|snapshot|fanout new|fanout pick|inbox` — JSON with `--json`, human table otherwise. `attach` streams output to the terminal (raw mode) until Ctrl-\.

## Pearl rail data

`GET /api/flow/sessions/{id}/handoff` → `{pearl, handoff:{worktree, branch, head, dirty:[paths], agent_session_id, next}, checkpoints:[{at, note, auto:bool}], blocks:[ids], pr:{number,url,ci}|null}`. `handoff` and `checkpoints` come from `th pearls prime --json`/`th pearls checkpoint` (lane C); the engine shells out to `th` for them.
