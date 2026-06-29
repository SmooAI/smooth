---
'smooai-smooth-daemon': patch
---

EPIC th-c89c2a (th-2ff975): `smooth-daemon schedule add/list/remove` — the CLI to
manage proactive schedules. `add "<prompt>" --every 30m|2h|90s|1d` or `--daily-at
HH:MM` writes a `Schedule` to the durable store the running daemon's tick loop
reads; `list` shows cadence + next-due + prompt; `remove <id>` deletes. Reachable
as `th daemon schedule …` (passthrough). This closes the loop: a user can now
create an always-on proactive task that fires into the operator on its cadence.
