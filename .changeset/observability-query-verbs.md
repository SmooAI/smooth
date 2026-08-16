---
'@smooai/smooth': minor
---

`th api observability` can now read the telemetry it always claimed to: `logs query/facets/fields/trace`, `traces query/show`, `errors top/show`, `llm turns/tool-failures/cost`, and `health` (pipeline freshness). Every verb takes `--json`, including the two sourcemaps verbs. Windows are relative and unit-suffixed (`90s`, `45m`, `6h`, `7d`); a bare number is rejected rather than guessed at.

The same query layer backs nine new read-only `observability_*` tools in `th mcp serve` — logs, traces, errors, service health, monitor incidents, audit trail, and LLM turns/tool-failures/cost — so a coding session can diagnose the running system instead of guessing. Throughout, "the query ran and matched nothing" and "the read failed" are worded differently on purpose, a full page always reports that there may be more, and unmeasured LLM cost renders as "not measured" rather than $0.00.

`th audit list`/`tail` now pick up `.jsonl` streams and default to the most recently written one. They matched only `<actor>.log` and defaulted to the long-removed `leader` actor, which made `egress-proxy.jsonl` — the only audit stream anything writes today — unreachable.
