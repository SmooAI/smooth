---
"@smooai/smooth": minor
---

feat: harden pearls sync — auto-push mutations + fail-safe pull (no silent data loss)

Pearls could be lost to the `refs/dolt/data` divergence: a mutation committed
only to the local Dolt store, then a later `th pearls pull` moved `main` to the
remote tip and orphaned the un-pushed commits. Two guards close the gap
(pearl th-4a4559):

- **Auto-push on mutation.** `th pearls create/update/close/reopen/dep/comment/
  label/migrate` now push to the repo's `refs/dolt/data` immediately after the
  local commit — best-effort and quiet when there's no remote/offline (drives
  only `dolt push`, which captures its own output; no stray `fatal:` on stderr).
  Pearls are durable the moment they're made, so no pull or re-clone can drop
  them. `SMOOTH_PEARLS_NO_PUSH=1` opts out (bulk/scripted creates).
- **Fail-safe `th pearls pull`.** Refuses by default when local `main` is ahead
  of `remotes/origin/main` (commits not yet on the remote), pointing you at
  `th pearls push` first; `--force`/`-f` pulls anyway. Detection fetches the
  remote and counts `dolt_log('remotes/origin/main..main')`; if it can't be
  determined (no remote / fetch fails) the guard is skipped so remote-less
  stores still pull.

Generalizes the messaging sync helpers (`sync_push_pearl_state` /
`sync_pull_pearl_state`, formerly `*_messaging`) since they push/pull the whole
pearl store. Verified live: a remote-less `create` is a clean no-op, and a
store that was 2 commits ahead correctly refused the pull.
