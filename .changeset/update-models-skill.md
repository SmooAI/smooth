---
'@smooai/smooth': patch
---

skill: `/update-models` — refresh model strings + pricing, and probe what actually works

Smooth names models in six places (the Settings picker, routing aliases, providers.json
defaults, the `th code` picker, `operator serve`'s default, the daemon's FAST_MODEL) and
they drift from what llm.smoo.ai serves. The skill pulls the live catalogue with pricing,
tier, and tool support from `/model/info`.

The part that earns it: it **probes**. A model being listed does not mean it works —
`gpt-5.5` was catalogued, priced, and advertised tool support while being completely
unusable through Big Smooth (it rejects `temperature: 0`, which the daemon sends, so
every call 400'd and the assistant silently said nothing). Probing catches that class
before it reaches the picker.
