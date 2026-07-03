---
'@smooai/smooth': minor
---

Drive the `th code` `/model` picker from the gateway's live `GET /v1/model/info`
(pearl th-7ee88e, SMOODEV-1793) — the last piece of the migration off the
gateway's removed `smooth-*` slot aliases.

The picker previously sourced its catalog (use-cases, tier, $/M cost,
benchmarks) from a baked offline table. It now fetches the gateway's live
`/v1/model/info` at startup (`fetch_gateway_catalog` +
`parse_gateway_model_info` in `smooth-code/src/model_picker.rs`) and treats it
as authoritative: models the gateway has removed drop out, new ones appear, and
metadata comes from the gateway without a Smooth release. Falls back to the
offline catalog when no gateway is reachable, and folds in local providers'
live models either way.

`slot_use_cases` is broadened to the gateway's real use-case taxonomy (which has
no `judge`/`summarize`/`guardrails` tags) so the Judge/Summarize/Fast slots
still admit the models they default to rather than filtering to empty against a
live catalog.

The concrete-model routing, `providers.json` migration shim, and defaults table
already landed under this pearl; this completes the info-driven picker. Docs:
Using-th-CLI "Routing slots & the model catalog".
