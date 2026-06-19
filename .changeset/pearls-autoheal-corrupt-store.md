---
"@smooai/smooth": patch
---

pearls: auto-heal a corrupt/unreadable Dolt store + fix the clone that left `main` empty

Two linked fixes for the pearl-store corruption class that left the smooai
monorepo's store unreadable (`open .../.dolt/repo_state.json: no such file or
directory`), independent of any pearl work.

**Root cause (th-3f6657) — `smooth-dolt clone` left `main` at the empty init
commit.** `cmdClone` did init + remote-add + `DOLT_PULL origin main`. The init
root is always unrelated to the remote's history, so `DOLT_PULL` fetched all
chunks into `remotes/origin/main` but refused to merge unrelated histories,
silently leaving `main` on the empty init commit. Every fresh bootstrap clone
came up "empty" (`table not found: pearls`) while physically holding the full
pulled data. `cmdClone` now force-resets `main` onto the pulled remote head
after the pull (no-op when the remote branch is absent).

**Auto-heal (th-03cdb8) — wire recovery into the `th pearls` open path.** Any
`th pearls` command now recovers on open instead of surfacing a raw smooth-dolt
error:

- `SmoothDolt::diagnose` now classifies a `.dolt/` dir that's missing
  `noms/manifest` or `repo_state.json` as recoverable `Corrupt` (the
  interrupted-GC/half-clone signature) rather than dead-end `NotInitialized`.
- `recover_from_remote` resolves the origin from the enclosing git repo's
  `origin` when `repo_state.json` itself is the missing file, and normalizes
  the root/`pearls`-subdir layout so the re-clone lands correctly. It reuses
  `clone_from` (so it inherits the clone-reset fix above).
- `PearlStore::open` runs the recovery on first-touch failure (snapshot the
  broken store aside, re-clone from origin, re-open), loudly to stderr.
  CLI-mode only — it never re-clones out from under a live `smooth-dolt serve`
  (Big Smooth); those cases point to `th pearls doctor --force`.

Canonical pearl data lives on the remote's `refs/dolt/data` under the beads
model, so the re-clone is non-destructive. Covered by new unit tests for the
diagnose classification and the git-origin fallback.
