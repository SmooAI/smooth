---
"@smooai/smooth": patch
---

th config set: consistency + hardening (pearl th-7ea946)

Brings `set` in line with `get`'s flag surface, plus four hardenings:

- **`--json`**: emits the API response as JSON, mirrors `get --json`.
  JSON output is never masked — caller asked for the wire shape.
- **`--reveal`**: opt-in plaintext echo on `set` and `list`. Mask is
  the default (pearls th-4ebbf7 + th-9cc412); `--reveal` mirrors
  `scripts/secret-helpers/sst-secret-list --reveal` (CLAUDE.md §13).
- **`--tier` as `ValueEnum`**: `public` / `secret` / `feature_flag`
  validated at parse-time. Typos like `--tier=secrets` now error
  with a list of valid options instead of round-tripping to the API
  and failing with a less-actionable 4xx.
- **Empty-value reject**: `th config set FOO ""` (or whitespace-only)
  fails at parse-time with `value cannot be empty or whitespace-only`,
  not silently after the API call.

Drops the `DEFAULT_TIER` `&str` constant in favor of `Tier::default()`
so the default tier and its wire format are colocated. Tier wire
format is locked by a test so a snake_case → camelCase regression
can't sneak past.
