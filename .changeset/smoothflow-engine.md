---
'@smooai/smooth': minor
---

SmoothFlow engine + `th flow` (epic th-6ac036, lane A th-7f0af3). New
`smooth-flow` crate hosted in `smooth-daemon`: agent/shell sessions run under
one long-lived `tmux -L smooth-flow` server so they outlive the daemon and the
app; PTY bytes stream to attached clients over `GET /api/flow/ws` (a
`portable-pty` on `tmux attach`, so late joiners get a full redraw and resize
works); Claude Code hooks (`POST /api/flow/hooks`) drive state, with a 120 s
long-poll for `PermissionRequest`; a supervision tick resumes crashed agents
with backoff, schedules usage-limit resumes at the parsed reset time, and
refuses duplicate resumes; fan-out races N worktrees and merges the winner.
The Smoo Relay bridges `channel:"flow"` envelopes to the flow WS with phone
caps (16 KiB / ~30 fps). `th flow ls|new|attach|send|approve|kill|snapshot|
inbox|handoff|fanout` is the thin CLI. The Claude Code pane-state heuristics
moved from `smooth-cli` into `smooth-tmux::detect` so `th claude` and the
engine share one copy.
