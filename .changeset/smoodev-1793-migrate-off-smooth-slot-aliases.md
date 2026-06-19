---
"@smooai/smooth": minor
---

SMOODEV-1793: migrate Smooth off gateway `smooth-*` slot aliases

The Smoo AI LLM gateway is removing the `smooth-*` semantic-slot
aliases (`smooth-coding`, `smooth-reasoning`, `smooth-reviewing`,
`smooth-judge`, `smooth-summarize`, `smooth-fast`, `smooth-default`,
plus deprecated `smooth-planning` / `smooth-thinking` and the various
`smooth-<slot>-<vendor>` sub-aliases). After cutover, any request for
those model names returns HTTP 400 `Invalid model name` from the
gateway.

What changes:

- **New mapping table** in `smooth_policy::smooth_alias` is the single
  source of truth for legacy → concrete model rewrites:

  | Old slot | Concrete model_name |
  |---|---|
  | `smooth-coding` / `smooth-default` | `deepseek-v4-flash` |
  | `smooth-reasoning` (+ planning/thinking) | `deepseek-v4-pro` |
  | `smooth-reviewing` | `minimax-m2.7-direct` |
  | `smooth-judge` / `smooth-summarize` | `gemini-2.5-flash` |
  | `smooth-fast` | `gemini-2.5-flash-lite` |

- **Migration shim** in `smooth_cast::provider_migration` walks every
  routing slot on a loaded `ProviderRegistry` and rewrites legacy
  aliases in place. `load_providers_with_migration(path)` is a drop-in
  replacement for `ProviderRegistry::load_from_file` that loads,
  migrates, **saves the file back if anything changed**, and emits one
  `tracing::info!` per rewrite so users see the migration once.

- **Every `providers.json` loader** in the workspace funnels through
  the migration loader (smooth-cli, smooth-bigsmooth, smooth-code,
  smooth-bench, smooth-operative — 31 call sites total). Existing
  users' on-disk configs are rewritten on first load; the in-memory
  migration also covers routing JSON shipped to operatives so older
  Big Smooth builds can still drive a freshly-built operative.

- **The TUI model picker** drops the hardcoded `SMOOTH_ALIASES` array
  and now offers the concrete catalog defaults. The picker also
  surfaces metadata (use-case tags, tier, cost, benchmark) sourced
  from the gateway's `/v1/model/info` schema (offline fallback
  catalog colocated in `smooth-code/src/model_picker.rs`).

- **`th model login`** no longer offers the dead `smooth-*` aliases
  for the SmooAI Gateway provider.

Coordination: the SmooAI-side gateway change (LiteLLM config) can roll
out once this branch lands on Smooth `main`, is rebuilt, and reinstalled
via `pnpm install:th`.
