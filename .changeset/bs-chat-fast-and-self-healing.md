---
"@smooai/smooth": minor
---

Big Smooth chat: faster, self-healing, more visible

**Speed**

- Chat agent default model flipped from `smooth-reasoning-kimi` (slow)
  to `smooth-coding` (MiniMax — fast AND tool-call-capable). Cuts the
  end-to-end "do I have a github repo for X" round trip from 60–90 s
  (teammate spawn) to ~25 s (direct bash).
- New `bash` tool on the chat agent with a tight read-only allowlist
  (`gh git kubectl jq curl ls cat head tail wc grep rg fd find echo`),
  so simple lookups don't need to spawn a teammate. System prompt
  re-written to bias toward `bash` for one-shot lookups.
- `teammate_wait` poll cadence dropped from 5 s → 1.5 s so the chat
  agent picks up `[IDLE]` / `[CHAT:TEAMMATE]` within one round-trip
  of the teammate posting it.
- Thought summarizer concurrency raised 2 → 4 so bubble bursts surface
  faster.

**Self-healing**

- `SmoothDoltServer::spawn` now retries once after killing zombie
  `smooth-dolt serve` processes for the same data dir, fixing the
  recurring "did not create socket within 15 s" startup failure.
  Timeout bumped 15 → 30 s.
- Global pearl store (`~/.smooth/dolt`) now uses the long-running
  `smooth-dolt serve` companion instead of per-call CLI subprocesses,
  dodging the Dolt manifest-lock races that produced "database is
  read only" errors when the chat handler tried to save messages.
- `run_cli` captures stderr inline so failures surface a real reason
  instead of "rerun the CLI for stderr".
- `coding_workflow::snapshot_workspace` refuses to recurse when the
  workspace looks like `$HOME` (or contains classic HOME children
  like `Library`/`Desktop`/`Documents`, or has > 200 top-level
  entries). Closes the runaway-copy hang that wedged direct-mode
  teammates whose `working_dir` defaulted to BS's cwd.

**Direct-mode UX**

- Orchestrator background loop is skipped when
  `SMOOTH_WORKFLOW_DIRECT=1`. Stops it from independently spawning
  microsandbox VMs (via Bootstrap Bill) for ready pearls when the
  rest of the system is meant to be sandbox-free.

**Big Smooth face**

- Sunglasses (two slim lenses + bridge + top frame + lens flash),
  fedora-style hat (crown + brim + teal hat band), and a thicker
  smirk mouth. Mouth opens a hair while thinking; a "lens flash"
  glimmers across the shades every couple seconds for cool factor.
  Face also bigger — 96 px on desktop (was 72 px).

**Thought-bubble UI**

- Bubbles moved to their own row beneath the title for visibility,
  with a green-tinted container so the row is obvious even before
  the first thought lands. TTL bumped 7 s → 14 s; bubbles persist
  after the reply so the user can read what BS was thinking.
- "Big Smooth is thinking · · ·" placeholder bubble shown while
  streaming with no thoughts yet, with animated dots.
- New `[Stop]` button replaces `[Send]` while the chat agent is
  in flight, with an `AbortController` so the user can reclaim
  the input if a long-running call gets stuck.
- Heartbeat thoughts: when no new tool-call event has fired for
  ≥ 8 s, the streamer emits a fresh "still working" summary every
  ~11 s so a long `teammate_wait` doesn't leave the bubble row
  silent.
