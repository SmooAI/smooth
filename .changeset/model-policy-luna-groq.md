---
'@smooai/smooth': minor
---

Big Smooth, `th` and every model picker now run on the `gpt-6-luna` family or Groq (SMOODEV-3342 model policy):

- **Slot defaults:** reasoning `deepseek-v4-pro` → `gpt-6-luna-high`, reviewing `minimax-m2.7-direct` → `gpt-6-luna`, summarize `gemini-2.5-flash` → `gpt-6-luna`, fast `gemini-3.5-flash` → `gpt-6-luna-fast`. Coding/default stay `gpt-6-luna`; the judge stays `groq-gpt-oss-120b`.
- **Pinned configs move too.** A `providers.json` slot on the Smoo gateway that still holds a retired default (`gpt-5.6-luna`, `deepseek-v4-flash`, `deepseek-v4-pro`, `minimax-m2.7-direct`, `gemini-2.5-flash`, `gemini-3.5-flash`, `gemini-2.5-flash-lite`) is rewritten on load and saved back. Changing the default did not reach these configs before, which is why `th` kept sending `gpt-5.6-luna` after the switch to `gpt-6-luna`. Slots on other providers are left alone. Big Smooth now runs this migration at startup; before, it read `providers.json` raw.
- **The model picker offers only** `gpt-6-luna`, `gpt-6-luna-fast`, `gpt-6-luna-high`, `gpt-6-sol` (the one premium, explicit high-quality choice), `groq-gpt-oss-120b`, `groq-gpt-oss-20b` and `groq-qwen3.8-27b`. `docs/model-scores.json` is filtered to that set. Offered models without a bench score are listed as "not yet benched", never with an invented number. The full bench board moves to `docs/model-leaderboard.json`.
- `th model login`'s gateway catalog, `th`'s agentic-harness default and the `smooth-bench` defaults (model, driver, judge) move from `deepseek-v4-flash` to `gpt-6-luna`. The weekly leaderboard now scores the offered set. The daemon's last-resort model is `gpt-6-luna` instead of `claude-haiku-4-5`.
