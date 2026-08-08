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


## Prefer the latest — with two hard gates

Default to the newest version in each family. Model families move fast
and a picker pinned to last quarter's release is quietly paying more for
less. `probe-models.py` lists everything the gateway serves, and family
names sort usefully (`minimax-m2.7` < `minimax-m3-direct`,
`glm-5.1` < `glm-5.2-direct`, `kimi-k2.5` < `kimi-k2.7-code-direct`).

Two things stop a swap, and neither is negotiable:

**1. It must be priced.** Several models are routable and tool-capable
but carry **no cost in the LiteLLM config**, so the gateway reports
`x-litellm-response-cost: 0`. Routing to one means serving traffic that
cannot be billed or attributed, and the bench renders its cost as `—`
rather than `$0` because a zero there is a missing measurement, not a
free model. Check before promoting:

```bash
python3 .claude/skills/update-models/probe-models.py | awk '$2==0 && $3==0'
```

As of 2026-08-08 that list is `glm-5.2-direct`, `glm-5-turbo-direct`,
`qwen3-max-direct`, `qwen3.5-plus-direct`, `qwen3.5-flash-direct`,
`qwen3.6-flash-direct`. The config carries a `# TODO: pricing not in
LiteLLM catalog yet — set manually before traffic` for exactly this.
Price it in `apps/k8s/apps/litellm/config.yaml` (smooai repo, via
`/litellm-model-refresh`) first.

**2. It must beat the incumbent on the bench.** "Newer" is a hypothesis,
not a result. Run the suite before swapping:

```bash
smooth-bench convo --model <incumbent> --model <candidate> --trials 3 \
  --scoreboard board.json
```

Read the **tool-use block**, not just the pass rate — a model can match
on outcomes while taking three times the tool calls with a higher error
rate, which is the difference between an agent you can leave running and
one you cannot.



> [!warning] Do not probe during a rollout — you will get a false BROKEN
> A litellm `config.yaml` change ships as a kustomize ConfigMap whose
> hash forces a rolling restart, so for a few minutes some pods serve the
> old `model_list` and some the new. `/v1/models` lists the new model
> immediately while `/chat/completions` returns
> `Invalid model name passed in model=…` for whichever share of traffic
> lands on an old pod — measured at 5/10, then 3/6, then 6/6 as it
> converged (th-66c65b).
>
> A single `--probe` call in that window reports a perfectly good model
> as `BROKEN`. Confirm convergence first:
>
> ```bash
> for i in $(seq 1 6); do
>   curl -s -o /dev/null -w "%{http_code} " -X POST https://llm.smoo.ai/v1/chat/completions \
>     -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
>     -d '{"model":"<new-model>","messages":[{"role":"user","content":"hi"}],"max_tokens":3}'
> done; echo
> ```
>
> Six 200s means every pod has it. Anything mixed means wait.

## Cost bracket before capability

Check the price before you spend a benchmark run on a model. The suite
costs real money per model, and a model outside the bracket you are
willing to pay is not a candidate no matter how it scores.

Measured 2026-08-08, against `deepseek-v4-flash` at $0.14/$0.28:

| bracket | models | vs flash |
| --- | --- | --- |
| **workhorse** (the target) | `deepseek-v4-flash`, `groq-gpt-oss-120b`, `minimax-m3-direct`, `qwen3.6-flash-direct`, `qwen3.5-plus-direct` | 1–3x |
| mid | `glm-5.2-direct`, `kimi-k2.7-code-direct`, `qwen3-max-direct` | 6–10x |
| premium | `gpt-5.5`, `kimi-k3-direct`, `claude-opus-4-8` | 20–100x |

**Having a model in LiteLLM is not the same as routing to it.** Keep the
premium tier configured and priced — it costs nothing until called, and
an unpriced model is unbillable (see the gates above). But do not spend
bench runs proving what a price sheet already tells you: `kimi-k3-direct`
at $3.00/$15.00 is a gpt-5.5 alternative, not a flash alternative, and if
nobody intends to pay gpt-5.5 rates then neither is a candidate.

Bench the bracket you would actually ship.

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
