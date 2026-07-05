# smooth-agent

A Claude Code plugin for **Big Smooth orchestration** — drive Claude Code worker
sessions that survive the account-wide rate-limit throttle, coordinate them over
**th-mail**, and track work as **pearls**. Part of the `smooth` marketplace
(`SmooAI/smooth`).

## What it gives you

- **`/smooth`** command — the orchestrator surface. `run`, `add-agent`, `drive`/
  `manual`, `mail`, `status`, `ls`, `attach`. Drives the `th claude` engine.
- **`agent-comms`** skill — teaches a worker session to report status, answer
  pings, and hand off work over `th msg`/`th agent`.
- **`pearls-flow`** skill — teaches a worker to track work as pearls
  (`th pearls`).
- **SessionStart hook** — when a session is launched by `th claude run` (which
  exports `SMOOTH_AGENT_HANDLE`), auto-registers it on the th-mail bus so Big
  Smooth can address it by id.

## Requires

The `th` CLI (built from `SmooAI/smooth`) with the `th claude` engine, plus
`tmux` on `PATH`. The plugin is a thin recipe layer; the supervision, rate-limit
governor, and session control live in `th claude` (the binary).

## Install

```
/plugin marketplace add SmooAI/smooth      # or: /plugin marketplace add ./ from a local checkout
/plugin install smooth-agent@smooth
```

Then `th claude run "<task>"` launches a supervised, plugin-active worker, and
`/smooth status` shows the farm.

## How control works

Each worker runs in a tmux session shared between Big Smooth and you. A per-session
**mode** arbitrates who types:

- `driving` — Big Smooth sends input + rescues rate-limits.
- `manual` — you drive (`th claude attach <id>`); the supervisor only rescues
  your throttled turns.
- `paused` — the supervisor stands down.

Flip with `/smooth drive <id>` / `/smooth manual <id>` or `th claude mode <id> <mode>`.

## Note on scale (subscription ToS)

This drives Claude Code **subscription** auth. Backoff-and-resume that honors the
limit is fine; a large unattended fleet to maximize a flat-rate plan is the gray
zone — keep the worker count tasteful. True fleet scale belongs on the metered
API + smooth-operator.
