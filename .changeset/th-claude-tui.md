---
"@smooai/smooth": patch
---

th claude tui: ratatui control dashboard for supervised sessions

Adds `th claude tui` — a live dashboard listing supervised Claude Code sessions
with their mode and a snippet of each one's pane, plus single-key control:
`d`/`m`/`p` flip a session between driving / manual / paused, `a`/`enter` attach
(suspends the TUI, hands the terminal to `tmux attach`, then restores), `r`
refreshes, `q` quits. This is the "switch between Big Smooth driving and the
session itself" surface from the orchestration plan. The key bindings, selection
clamping, pane tailing, and list navigation are pure and unit tested; the draw +
event loop is the IO shell.
