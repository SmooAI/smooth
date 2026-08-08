---
'@smooai/smooth': patch
---

Settings picker: Code and UI move to benchmarked models

`Code` was `minimax-m2.7` (superseded, and m3 scored 66.7%) → `qwen3.7-plus-direct`,
the best model measured at any price: 84.4% on the 15-scenario convo suite at 3 trials,
reached with 43% fewer tool calls than Flash.

`UI` was `glm-5.1` → `glm-5.2-direct`, the current release — it was unpriced in LiteLLM
until today, which is why it had never been benchable.

`Flash` stays `deepseek-v4-flash`: cheapest per passing scenario by 2.6x, and it is the
default every session lands on. Premium slots stay put deliberately — GPT-5.6 is newer
but unbenchmarked, and its one measured run scored 0/45 because every tool call 400'd.
