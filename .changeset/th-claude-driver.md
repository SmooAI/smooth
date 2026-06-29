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

This is the 1:1 vertical slice of a broader topology (1→N Big-Smooth-led farm,
N→1 per-session supervisors, and mixed), all built on the same
supervisor + governor + registry primitives. The governor is shared so a 429 on
any session backs off the whole pool rather than thundering the herd.
