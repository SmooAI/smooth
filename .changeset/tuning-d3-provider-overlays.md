---
"@smooai/smooth": patch
---

Per-provider operator-runner system-prompt overlays (opencode pattern)

The operator runner now prepends a short, model-family-specific overlay to
the base `system.md` before dispatching the LLM call. Adapted from
opencode's per-provider prompt directory (`anthropic.txt`, `beast.txt`,
`gemini.txt`, `kimi.txt`, …) but trimmed and re-tuned for the Smoo cast
vocabulary.

7 overlay files added at `crates/smooth-operator-runner/prompts/providers/`:

- `anthropic.md` — Claude family. Lean into long-form reasoning + tool
  precision; restraint rules apply *especially* hard since the family
  trends toward over-explaining.
- `gpt.md` — GPT/Codex/o-series. The big counter-failure-mode block:
  "keep going until completely resolved", "training data is out of date",
  "no half-finished implementations", "verify before claiming done."
- `gemini.md` — Gemini family. Native tool calls (no `tool_code` blocks)
  + long-window drift mitigation (re-read after each meaningful change).
- `kimi.md` — MiniMax / Kimi / `smooth-coding` default. Bias to action,
  smallest correct edit, build-then-claim-done.
- `deepseek.md` — `smooth-reasoning` slot. Plan-then-act, but reasoning
  isn't an excuse to skip verification.
- `glm.md` — Z.ai / GLM. Tool-call format precision, no over-elaborate
  preambles.
- `qwen.md` — Qwen. English-only output in code; native tool-call schema.

`crates/smooth-operator-runner/src/provider_overlay.rs` adds the loader:
- `for_model(&str) -> Option<&'static str>` returns the right overlay
  given a model identifier.
- Smoo semantic aliases resolve first (`smooth-coding` → kimi,
  `smooth-reasoning` → deepseek, `smooth-fast-gemini` → gemini,
  `smooth-judge` → anthropic, etc.) — pinned so a gateway routing flip
  doesn't silently change the prompt scaffold.
- Family substring fallback handles explicit model strings like
  `claude-haiku-4-5-20251001`, `kimi-k2-thinking`, `gpt-5.4-mini`,
  `gemini-3-flash`, `deepseek-v3.2-speciale`, `glm-5.1`, `qwen3-coder-plus`.
- Unknown models return `None` and the runner falls back to the base
  prompt unchanged — non-breaking for any unconfigured model.

`main.rs` system-prompt assembly prepends `provider_overlay::for_model(...)`
output before `system.md`. 5 unit tests cover alias resolution, family
substring matching, prefix-order safety (smooth-fast-gemini must hit gemini
not gpt), unknown-model fall-through, and overlay-content non-emptiness.

This is the prompt-side complement to the routing slot work — when the
gateway routes coding to Kimi, the runner now boots with Kimi-tuned
discipline rules rather than the generic base prompt.
