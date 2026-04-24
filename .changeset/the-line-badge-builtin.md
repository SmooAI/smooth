---
"smooai-smooth-cli": minor
"smooai-smooth-bench": patch
---

The Line is now visible in two new places:

- **README badge** — points at `docs/bench-badge.json` (Shields.io endpoint format), auto-updated on every release tag alongside `docs/bench-latest.json`. Thresholds: ≥80% brightgreen, ≥60% yellow, else orange. A partial-sample (budget-cap hit) shows a ⚠ suffix.
- **`th bench score`** — new subcommand prints The Line baked into this binary at build time. Reads `docs/bench-latest.json` via a `build.rs` rustc-env injection and formats with the same human table `smooth-bench score` uses (shared via `Score::render_table()`). When no Line is baked in yet it prints a hint explaining how to produce one locally.

Supporting changes: `scripts/the-line/render-badge.sh` (jq-based Shields endpoint renderer), wired into `.github/workflows/the-line.yml` + its dry-run harness. `Score::render_table()` in `smooth-bench` is now public so both the harness binary and the CLI can render identical tables.
