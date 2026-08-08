---
name: update-models
description: Refresh model strings, pricing, and availability across every place Smooth names a model — the Settings picker, routing aliases, providers.json defaults, the th code picker. Probes the gateway for what ACTUALLY works, not just what is listed. Use on "/update-models", "update the models", "refresh model pricing", "are our models current", "add <model> to the picker", or when a model in the picker misbehaves.
---

# Updating models across Smooth

Smooth names models in **six** places, and they drift from what
`llm.smoo.ai` actually serves. This skill refreshes them from the live
gateway — and, importantly, checks that each one still *works*.

## The rule that makes this skill worth running

**A model being in the catalogue does not mean it works.** Pearl
th-c127d1: `gpt-5.5` was listed, priced, and advertised
`supports_function_calling: true` — and was completely unusable through
Big Smooth. It rejects `temperature: 0`, which the daemon sends, so every
call 400'd and the user saw an assistant that silently said nothing.

A refresh that only updates strings would ship that straight back into
the Settings picker. **Always `--probe` before promoting a model.**

## 1. Pull the catalogue

```bash
# free — catalogue + pricing, no model calls
python3 .claude/skills/update-models/probe-models.py

# costs a few cents — one real tool-calling turn per model
python3 .claude/skills/update-models/probe-models.py --probe

# just the ones you care about
python3 .claude/skills/update-models/probe-models.py --probe \
  --only deepseek-v4-flash --only gpt-5.5

# machine-readable, for diffing
python3 .claude/skills/update-models/probe-models.py --json /tmp/models.json
```

Credentials come from `SMOOAI_GATEWAY_KEY`, else the `llm.smoo.ai`
provider in `~/.smooth/providers.json` — the same store the daemon reads.
Both endpoints it uses (`/model/info`, `/v1/chat/completions`) work with
an ordinary key; `/spend/logs` does not (admin-only), which is why
pricing comes from `/model/info`.

`status` column:

| status | meaning |
|---|---|
| `ok` | callable, returns tool calls, accepts `temperature: 0` |
| `ok (rejects temperature 0)` | works only at its default temperature — see th-c127d1 before shipping it |
| `EMPTY REPLY` | 200 with neither content nor a tool call — the "Big Smooth says nothing" failure |
| `BROKEN (…)` | the gateway rejected the call outright |

## 2. Update every surface

`rg '"deepseek-v4-flash"' crates/` finds most of it. The full list:

| File | What it names |
|---|---|
| `crates/smooth-web/web/src/modes.ts` | **the Settings picker** — Flash/Code/UI/Plan/Fast + the `+` premium tier |
| `crates/smooth-policy/src/smooth_alias.rs` | routing aliases (`smooth-coding` → a real model) + the legacy-alias migration table |
| `crates/smooth-cast/src/providers.rs` | `providers.json` routing defaults |
| `crates/smooth-code/src/model_picker.rs` | the `th code` model picker |
| `crates/smooth-cli/src/operator_serve.rs` | `SMOOAI_MODEL` default for `th operator serve` |
| `crates/smooth-daemon/src/operator.rs` | `FAST_MODEL` (the narc judge / cheap classifier) |

Keep them consistent: a model in `modes.ts` that no alias or provider
default knows about will render in the picker and fail at runtime.

## 3. What to check before promoting a model

- **`--probe` says `ok`.** Not `ok (rejects temperature 0)` unless
  th-c127d1 is fixed — the daemon sends a fixed temperature.
- **`tools` is `True`.** Every agent turn binds tools.
- **The price is what you think.** Tiers are not intuitive:
  `gpt-5.5-pro` is $30/$180 per M tokens against `deepseek-v4-flash` at
  $0.14/$0.28 — **214x input, 643x output**. Put that in the PR
  description when promoting anything into the premium tier.
- **Run the bench.** `cargo run -p smooai-smooth-bench -- convo --model
  <old> --model <new>` scores them against each other on real scenarios.
  Cheap models have repeatedly held their own; do not assume the
  expensive one wins. See [[Bench-Harness]].

## 4. Land it

Changeset, then the usual gates (`cargo fmt`, `cargo clippy`,
`cargo test`, `pnpm build:web` if `modes.ts` changed). Record the probe
table in the PR body — it is the evidence the new model actually works,
and it dates the pricing.
