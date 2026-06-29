---
"@smooai/smooth": patch
---

th claude: tmux-driven Claude Code session supervisor with a shared rate-limit governor

Adds `th claude run / ls / attach` plus a new dependency-light `smooth-tmux`
crate. `th claude run` launches Claude Code inside an isolated tmux session and
supervises it: when the account-wide "temporarily limiting requests" throttle
fires, it backs off with full jitter (via a pool-aware `RateLimitGovernor`) and
resends the last message until it lands — auto-detecting the last user message
from the pane when it didn't send it itself. `th claude attach <id>` hands your
terminal to the session; `th claude ls` lists live sessions and prunes dead
ones.

`th claude mode <id> driving|manual|paused` hands control back and forth between
Big Smooth and a human sharing the same tmux pane: `driving` = the supervisor
sends input and rescues throttles, `manual` = the human drives and the supervisor
only rescues their throttled turns, `paused` = the supervisor stands down. Worker
sessions are launched with `SMOOTH_AGENT_HANDLE` exported so they can register on
the th-mail bus.

This is the 1:1 vertical slice of a broader topology (1→N Big-Smooth-led farm,
N→1 per-session supervisors, and mixed), all built on the same
supervisor + governor + registry primitives. The governor is shared so a 429 on
any session backs off the whole pool rather than thundering the herd.

Also adds the **`smooth` Claude Code plugin marketplace** (`.claude-plugin/
marketplace.json`) with the **`smooth-agent`** plugin — a `/smooth` orchestrator
command plus `agent-comms` / `pearls-flow` worker skills and a SessionStart hook
that registers a worker on th-mail. The recipe layer over the `th claude` engine.
