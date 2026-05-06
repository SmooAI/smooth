---
"@smooai/smooth": patch
---

th pearls push: auto-set-upstream, `--force`, actionable error on diverged remotes

`th pearls push` exposed only the bare Dolt push, so first-time push
to a fresh remote failed with `fatal: The current branch main has no
upstream branch` and the user had to drop into raw `smooth-dolt sql -q
"CALL dolt_push('-u', 'origin', 'main')"` to recover. This pearl
files three sharp edges:

- New `PushOpts { force, set_upstream }` on `SmoothDolt::push_with`,
  re-exported from the crate. The bare `push()` stays as a no-flag
  shorthand for callers that don't care.
- `th pearls push --force` (`-f`) overrides remote history when the
  remote has only a stale `Initialize data repository` commit from a
  previous abandoned init. No more raw SQL detour.
- Auto-retry with `set_upstream = true` when the first push fails
  with "no upstream branch". Users don't need to know the flag exists.
- Friendlier error on "no common ancestor" — the bare Dolt message
  was unhelpful; now the CLI surfaces the two real recovery paths
  (force push, or `git push origin --delete refs/dolt/data` then
  push) with a one-liner inspection command for the curious.
- Tightened `is_no_remote_error` so it only matches "no configured
  push/pull destination" — "no upstream" used to live there, which
  meant the global pearl store silently swallowed first-push errors
  instead of recovering with `-u`.
