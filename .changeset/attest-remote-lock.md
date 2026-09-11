---
'@smooai/smooth': patch
---

th attest: serialize remote runs so two agents can't corrupt the shared build box

The remote attest host has ONE worktree and ONE cargo target, but agents attest
concurrently (measured live: two `th attest rust` against the same box). Without a
mutex their `git checkout` / `git clean` / cargo runs stomp each other — one run's
clean deletes the other's in-flight tree — producing phantom failures (th-983292).

`remote_script` now takes a `mkdir` lock (the portable mutex; macOS has no `flock`)
before the checkout and releases it via a trap on exit, so exactly one run touches
the worktree at a time. A crashed holder's lock (its trap never fired) is broken once
it's older than any real run; the wait is hard-capped so a wedged holder reads as a
busy box (exit 97) rather than hanging forever (th-7db71c, waiter side). The check now
runs as `bash` not `exec bash`, so the release trap actually fires. A `bash -n` test
guards the hand-rolled locking against a syntax slip.
