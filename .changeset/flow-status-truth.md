---
'@smooai/smooth': minor
---

SMOODEV-4268: SmoothFlow's "who needs me" status holds up across Claude Code, Codex and OpenCode, on the tmux and pty hosts. Codex and OpenCode approvals and OpenCode questions now reach `needs_you`, and first-run dialogs read as waiting on you instead of `working` or `idle`. A prompt denied at the terminal or a turn interrupted with Esc no longer leaves a row stuck, and a resumed session no longer sits in `starting`. A state nothing has confirmed for a while shows as `working?`, dimmed, and an ask shows how long it has waited. Existing Codex users are asked once to review the changed hooks.
