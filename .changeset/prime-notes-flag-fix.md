---
"@smooai/smooth": patch
---

Fix `th prime` pearls docs: `th pearls update` never had a `--notes` flag. The stale primer (embedded via `include_str!`) re-injected the bad flag on every session start/compact, so agents kept trying `--notes`, hitting the CLI rejection, and emitting a reconciliation note every session. Replaced with the real editable fields (`--priority/--assign`).
