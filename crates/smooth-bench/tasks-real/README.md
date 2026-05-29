# tasks-real — curated real-world tasks for `smooth-bench score-real`

Each subdirectory is one task. Layout:

```
<task-id>/
  README.md           # task prose, shown verbatim to the human driver
  workspace/          # starting code, copied to the scratch dir
  hidden-tests/       # held-out tests, overlaid after TASK_COMPLETE
  grade.toml          # multi-axis config (weights, baselines, verify cmd)
```

See `crates/smooth-bench/src/grade.rs` for the `grade.toml` schema and
`crates/smooth-bench/src/score_real.rs` for axis semantics.

## Tasks

- **rust-ttl-cache** — wrap an existing `reqwest`-style client with a
  TTL cache. Bug: existing eviction is O(n); fix + add a cache layer.

## TODO — proposed but not yet written

- **rust-config-merge** — fix a TOML config merger that drops nested
  keys + add an env-var override layer (4 files).
- **rust-cli-flag** — add `--dry-run` to a small file-mover CLI; bug
  where `-v` and `--verbose` diverge (3 files).
- **python-invoice-dates** — fix tz-naive date parsing in an invoice
  CLI; add `--fiscal-year-start` flag (3 files).
- **ts-react-pagination** — fix off-by-one in a paginated list hook;
  add keyboard navigation (4 files).
