---
"@smooai/smooth": patch
---

Add `smooth-dolt-launcher` — clean-slate exec wrapper for spawn isolation

Tiny C binary (~5 KB, ~30 lines) that runs BEFORE Go starts:
resets the inherited signal mask, closes every fd > 2, `setsid`s,
then `execv`s the requested program. Used transparently when
`SmoothDoltServer::spawn_handle_once` launches `smooth-dolt serve`
from inside Big Smooth's Tokio runtime.

Without the launcher the child Go runtime can wedge on first SQL
query in pearl `th-1a61a7`-style failures: Tokio installs blocking
signal masks (Go needs SIGURG for goroutine preemption) and
contaminates fd inheritance (Go grabs leftover Tokio epoll fds at
startup). Restored daemons via this path get clean process state.

The launcher is opt-in via path discovery — falls back to the
shell-laundered spawn if the binary isn't installed alongside
`th` and `smooth-dolt`. CLI invocations of `th pearls *` and
short-lived parents work without it; long-running daemons
(BS) benefit from it.

Build: `bash scripts/build-smooth-dolt-launcher.sh`
