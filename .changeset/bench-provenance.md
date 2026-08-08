---
'smooth': patch
---

bench: record which binary was actually benchmarked

The Rust engine boots `th daemon` from PATH, and `th daemon` runs the separate
`smooth-daemon` binary, also from PATH. `prepare_engine` rebuilds go/ts/python but
Rust falls through, so the reference implementation — the one every other engine is
compared against, and the one the published leaderboard uses — was whatever happened
to be installed.

Runs now print the resolved binaries and `th --version`, and warn when it was not
built from the checkout's HEAD. The warning fired on the first real run, against a
binary from the commit the session started on. Same class as the stale TypeScript
`dist/` (th-11284c), wider blast radius.
