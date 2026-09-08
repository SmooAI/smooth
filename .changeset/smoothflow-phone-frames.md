---
'@smooai/smooth': minor
---

SmoothFlow v0.1 frames for the phones (th-d33afa, epic th-6ac036). The engine
now keeps a per-session event stream — `flow.event {id, event_id, at, kind:
user|agent|tool|system, text}` derived from Claude Code hooks, `flow.send`,
`flow.approve` and every state change — buffered to the last 200 in
`flow.db` and replayed on `flow.attach`. `flow.handoff {id}` answers over the
WS with the pearl-rail packet (`th pearls show --handoff --json` when the
installed `th` has it). A client `flow.hello` re-sends the hello, and the
relay bridge opens the flow WS on the first `channel:"flow"` envelope. Sessions
record the tmux socket they run on: `smooth-daemon operator --tmux-socket`
(`SMOOTH_FLOW_TMUX_SOCKET`) and `th flow new --tmux-socket` pick the server,
which on macOS decides whether agents inherit the SmoothFlow app's TCC grants.
