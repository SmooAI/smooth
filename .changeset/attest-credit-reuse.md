---
'@smooai/smooth': minor
---

`th attest` reuses a pass when nothing the check reads has changed. Each pass is recorded locally, and a later commit whose diff (and merge-base move) the repo's `scripts/ci/_rows-touched.sh` calls `untouched` for a check is credited without re-running it. Any `scripts/ci/` change, a missing or failing hook, a dirty tree or a pass older than 72h still runs the check. `--no-reuse` forces a run.
