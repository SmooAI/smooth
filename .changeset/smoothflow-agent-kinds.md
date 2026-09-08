---
'@smooai/smooth': minor
---

SmoothFlow runs OpenCode and Codex sessions first-class (th-5c5457, th-b423aa,
th-069c9e). Per-kind launch table: `opencode --prompt <p>` (the positional was
its project dir, so the prompt exited at once as "done") and `codex <p>`;
restore per kind (`opencode --session <id>`, `codex resume <id>`) once the
harness session id is known — the smooth-agent OpenCode plugin now posts every
lifecycle event to `/api/flow/hooks` with the Claude event names, and the
engine binds an id-less row by cwd on the first event. `Session.state_source`
(`hooks` | `inferred`) says how state is derived and `th flow ls` shows it as
VIA; pane scraping learned OpenCode's working/idle lines and Codex's menus.
Agent binaries are resolved past cmux's `cmux-cli-shims` (which inject their
own `--session-id` and hooks) to the real installs, recorded as `argv[0]`.
