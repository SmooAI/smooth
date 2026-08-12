---
'@smooai/smooth': patch
---

Route every model surface off the same benchmark, and drop gpt-5.5

The Settings picker was repicked from the 3-trial bench, but Smooth names
models in six places and the other five still pointed at the old lineup.
The worst of it: the `smooth-fast` routing alias still resolved to
`groq-gpt-oss-20b` — the model retired from the picker for scoring 17.9%
and being, in a slot named Fast, the slowest model measured. A picker
entry with no matching alias renders fine and fails at runtime.

Now consistent across `smooth_alias.rs` (slot defaults + the legacy
migration table), `operator_serve.rs` (`SMOOAI_MODEL` default),
`provider_migration.rs` and `model_picker.rs`:

- `smooth-coding` / `smooth-default` → `gpt-5.6-luna` (was
  `deepseek-v4-flash`): 89.3% vs 75.0%, about half the cost per pass.
- `smooth-fast` → `gemini-3.5-flash` (was `groq-gpt-oss-20b`).
- The legacy `groq-llama-3.1-8b` mapping retargets to the current fast
  default rather than landing an old config on a retired model.

`smooth-judge` stays on `groq-gpt-oss-120b` — a different job (the narc
classifier) and not part of this suite.

**`gpt-5.5` is dropped from the premium tier.** It scored 85.7% — below
the free-tier default — for $10.21 against luna's $0.013 on the same 28
scenarios. There is no reading of that trade that favours it, and
re-benching it only spends money to re-learn it. Premium is now two
slots: `code+` (claude-fable-5) and `max` (gpt-5.6-sol-high).

Every model in the lineup was re-probed against the live gateway before
landing: all return `ok` with tool calling, including at `temperature: 0`
(the failure mode that made gpt-5.5 unusable through Big Smooth in
th-c127d1).
