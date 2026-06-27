---
'smooai-smooth-pearls': patch
---

Harden the Dolt store against a read-only WEDGE caused by a hung remote sync.

The Dolt remote sync (`smooth-dolt push`/`pull`) shells out to `git` to move
`refs/dolt/data`; that git child holds the noms `LOCK` for the whole transfer.
If the network to the remote stalls, the lock was held indefinitely and every
other writer of the store got `Error 1105: cannot update manifest: database is
read only` (reads still worked) — a hard wedge until the stuck process was
killed by hand.

Two changes:

1. **Prevention — bounded remote sync.** CLI-mode `push`/`pull` now run under a
   wallclock timeout (`run_cli_timed`). On timeout the stalled git child is
   killed (releasing the noms LOCK so local writes recover immediately) and a
   *retryable* "sync timed out" error is returned instead of wedging. Default
   30s, overridable via `SMOOTH_DOLT_SYNC_TIMEOUT_SECS` (`0` = unbounded).

2. **Recovery — self-heal clears a stalled sync child.** The read-only
   auto-doctor now also detects and clears a `git` process holding the store's
   noms LOCK whose parent is `smooth-dolt` (a stalled dolt-sync child),
   alongside the existing orphaned `smooth-dolt serve` case. The safety guard is
   preserved and tightened: an unrelated `git` (parent is a shell/IDE/CI runner)
   or any non-sync holder is refused. Kill escalation is SIGTERM → brief wait →
   SIGKILL.
