---
"@smooai/smooth": patch
---

bench: stop `current_commit_sha()` returning empty under the pre-push hook / CI

`current_commit_sha()` shelled out to `git rev-parse HEAD` while inheriting
the caller's git environment. Under the git pre-push hook (and some CI
checkouts) `GIT_DIR` / `GIT_INDEX_FILE` / `GIT_WORK_TREE` / `GIT_PREFIX` /
`GIT_COMMON_DIR` are exported, which made the child git print nothing (exit 0,
empty stdout) instead of the real sha. That empty string failed the
`current_commit_sha_returns_something_non_empty` test in the full `cargo test`
run — blocking every direct push (pre-push hook) and the Release (Changesets)
workflow, while passing in isolation and in PR Checks.

Fix: strip the inherited `GIT_*` vars before invoking git so it rediscovers the
repo from cwd, and treat empty stdout as the `"unknown"` sentinel (same as the
git-failure path) so the function never returns `""` — which also stops release
Scores from being tagged with an empty provenance string. Pearl th-e2cbc9.
