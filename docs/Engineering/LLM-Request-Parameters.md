# LLM Request Parameters

#engineering

> [!warning] If Big Smooth "says nothing", read this first
> An agent that boots fine, accepts your turn, and then produces **no reply
> at all** is almost never a prompt problem. It is usually every LLM call
> returning `400`, and the most common cause is `temperature`.

## The failure

A growing set of frontier models accept only their **default** temperature
and reject the request outright:

```text
Unsupported value: 'temperature' does not support 0 with this model.
Only the default (1) value is supported.
```

Nothing about the symptom points at the cause. The daemon logs
`gateway + model resolved`, the WebSocket handshake succeeds, the turn is
accepted — and then the assistant is simply silent. Pearl th-c127d1: this
made Big Smooth's **entire model picker a no-op**. Every model except the
default returned an empty reply, for weeks, and it read as "that model is
broken" rather than "we send a parameter it refuses".

## The rule

**Never hardcode a temperature.** Use the one constant:

```rust
use smooth_policy::llm_params::AGENT_TEMPERATURE;   // 1.0
```

A unit test (`no_source_file_hardcodes_a_zero_temperature`) walks `crates/`
and fails the build if any `.rs` file writes `temperature: 0.0` / `0,`
again. It names the offending file and line. That guard exists because the
literal was written in **seven places across four crates**, and fixing six
of them looked exactly like fixing it.

## Why not a per-model allowlist

Because the behaviour does not follow the model names, so any table you
write from intuition is wrong. Measured by actually calling each model
against `llm.smoo.ai` on 2026-08-07:

| rejects `temperature: 0`                                                  | accepts it                                                                                                |
| ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `gpt-5.1`, `gpt-5.4-pro`, `gpt-5.5`                                       | `gpt-5`, `gpt-5.2`, `gpt-5.4`                                                                             |
| `claude-opus-4-7`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-fable-5` | `claude-haiku-4-5`, `claude-sonnet-4-5`, `claude-sonnet-4-6`, `claude-opus-4-6`                           |
|                                                                           | `gemini-3.5-flash`, `deepseek-v4-flash`, `deepseek-v4-pro`, `glm-5.1`, `minimax-m2.7`, `groq-gpt-oss-20b` |

`gpt-5.1` rejects while `gpt-5.2` accepts. `gpt-5.4` accepts while
`gpt-5.4-pro` rejects. There is no prefix rule, and the set moves every
time a provider ships a model.

`1.0` was accepted by **all 12 models tested across 6 families**. The cost
is losing temperature-0 determinism on models that would allow it — a fair
trade against "only the default model works at all".

## Re-measuring

Do not trust this table's date. The `/update-models` skill probes the live
gateway and reports `ok (rejects temperature 0)` per model:

```bash
python3 .claude/skills/update-models/probe-models.py --probe
```

## The real fix, upstream

`LlmConfig::temperature` should be `Option<f32>` so we send **nothing** and
take each provider's own default. That lives in `smooth-operator-core`.
Until it exists, `AGENT_TEMPERATURE` is how this repo stays consistent, and
`smooth-operator-server` carries the same constant for the turn config it
builds (`llm_config_with_key`).

## Debugging note that cost hours

`th daemon` runs the **separate `smooth-daemon` binary**, not in-process
code. Rebuilding `th` while debugging a daemon change changes nothing — a
stale `~/.cargo/bin/smooth-daemon` keeps serving. Build and run
`smooth-daemon` directly:

```bash
cargo build -p smooai-smooth-daemon --bin smooth-daemon
SMOOTH_ADDR=127.0.0.1:8790 SMOOTH_LOCAL_TOKEN=tok \
  SMOOTH_AGENT_MODEL=gpt-5.5 ./target/debug/smooth-daemon
```

Failed LLM requests are dumped in full to `~/.smooth/llm-errors/` — that
dump is what makes a parameter rejection visible at all.

## Related

- [[Bench-Harness]] — the suite that surfaced this
- [[Debugging-Guide]]
