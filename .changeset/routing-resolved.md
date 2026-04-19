---
"smooai-smooth-operator": minor
"smooai-smooth-cli": minor
---

Add `th routing resolved` — hits the LiteLLM `/model/info` admin endpoint on each configured provider and prints the alias → concrete-upstream map. Answers "what model actually runs behind `smooth-coding` today?" without needing server-side access. Internally exposed as `smooth_operator::resolution::{fetch_model_info, parse_model_info, ResolvedModel}` so other callers (bench harness, TUI status bar) can reuse it.
