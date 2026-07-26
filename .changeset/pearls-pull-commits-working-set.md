---
'@smooai/smooth': patch
---

Fix `th pearls pull` failing with "cannot merge with uncommitted changes"

`DOLT_PULL` merges, and Dolt refuses to merge a dirty working set — but
read-path commands dirty it as a side effect: `th msg inbox` and the
`th msg watch` poll loop heartbeat the `agents` table without committing.
That wedged `th pearls pull` on every invocation, and silently stopped
`th msg watch` from receiving remote messages at all (it swallows pull
errors, so each poll's own heartbeat blocked the next poll's pull).

`SmoothDolt::pull` now commits the working set first, via a new
`commit_working_set` helper that no-ops when the store is already clean
(`commit` passes `--allow-empty`, so an unconditional call would add an
empty commit per pull). Fixing it at that shared choke point covers every
caller, including interrupted commands that leave the store dirty.
