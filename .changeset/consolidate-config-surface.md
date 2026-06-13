---
"smooai-smooth": minor
---

th config + th admin config: consolidate config surface, delete th api config (pearl th-9c0c34)

Three surfaces collapsed into two:

- **`th config`** — daily-developer surface. Gains `feature-flag <key>`
  (evaluate a flag for the active org + env; pipe-friendly stdout —
  prints just `true`/`false`/string, or `--json` for the full envelope)
  and `delete <key>` (remove a value record; `--force` required for
  secret-tier). `--env` is now a long alias for `--environment` on
  every subcommand to save a keystroke.
- **`th admin config`** — platform-admin surface. New. Holds the
  infrequent verbs: `schemas` (list / show / create / update / delete
  / push / values), `environments` (list / create / update / delete /
  values), and `values bulk-set` + `values delete`. Same auth as
  `th config` (no `requireSuperAdmin` gate); the "admin" naming
  captures cadence + audience, not authorization level.
- **`th api config`** — **deleted entirely**. Nobody uses `th` yet so
  no aliases needed (per user direction 2026-06-13). The old
  `th api config values` overlapping `th config get/set/list` is gone;
  the old `th api config schemas`/`environments` lives at
  `th admin config`; `th api config feature-flag` lives at
  `th config feature-flag`.

Net: one daily surface, one admin surface, zero duplicate paths.
