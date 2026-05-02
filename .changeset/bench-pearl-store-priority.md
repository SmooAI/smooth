---
"@smooai/smooth": patch
---

bench: prefer `~/.smooth/dolt/` over repo-walked store

The bench's `locate_pearl_store_dir()` walked up from `cwd` first, so a
bench launched from `~/dev/smooai/smooth/` bound to
`~/dev/smooai/smooth/.smooth/dolt/`. The daemon, however, runs from
launchd at `$HOME` and creates pearls in `~/.smooth/dolt/`. The two
stores never met — the heartbeat task wrote `[PROGRESS]` comments to
the daemon's store while the bench polled an empty one, and the
600 s `idle_grace` always fired.

Resolution priority is now:

1. `SMOOTH_BENCH_PEARL_STORE` (explicit override)
2. `~/.smooth/dolt/` (the daemon's default — almost always correct)
3. Walk up from `cwd` for `.smooth/dolt/` (kept as a fallback for
   bench runs that explicitly target a project store)

Confirmed root cause via direct inspection of pearl `th-79c2d3` during
take 5: the heartbeat had written 5 `[PROGRESS]` comments at 30 s
intervals into the smooai project store, while the bench was polling
the smooth project store and never saw them.
