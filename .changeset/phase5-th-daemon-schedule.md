---
'smooai-smooth-cli': patch
---

Phase 5 (EPIC th-c89c2a): `th daemon schedule` — manage the always-on agent's
proactive tasks from the terminal. `th daemon schedule list` prints schedules
(id, cadence, next-due, prompt, disabled marker); `add --prompt … (--every-minutes N | --daily HH:MM)`
creates one (exactly one cadence flag required); `rm <id>` removes one. The
cadence-building and list-formatting are pure and unit-tested (flag parsing,
ranges, both kinds); verified live against a running daemon for the full
add/list/rm round-trip.
