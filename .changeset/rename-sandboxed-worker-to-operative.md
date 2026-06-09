---
"smooai-smooth": minor
---

rename the sandboxed-worker concept from "smooth-operator"/"operator" to "operative"

Disambiguates the microVM-per-pearl sandboxed worker (which RUNS the agent
engine) from the `smooth-operator` agent **engine** crate it consumes (being
extracted to `smooth-operator-core`) and the public `smooth-operator`
**service**.

Renamed worker identifiers (engine crate `smooth-operator` / `OperatorRole` /
all `proto/*.proto` `operator_id` wire fields are intentionally LEFT
UNTOUCHED):

- Runner crate/binary `crates/smooth-operator-runner` (pkg
  `smooai-smooth-operator-runner`, bin `smooth-operator-runner`) →
  `crates/smooth-operative` (pkg `smooai-smooth-operative`, bin
  `smooth-operative`). Engine dep `smooth-operator` kept as-is.
- Container image `ghcr.io/smooai/smooth-operator` →
  `ghcr.io/smooai/smooth-operative`; `docker/Dockerfile.smooth-operator` →
  `Dockerfile.smooth-operative`; `scripts/build-smooth-operator-image.sh` →
  `build-smooth-operative-image.sh`; `scripts/build-operator-runner.sh` →
  `build-operative.sh`.
- Env vars `SMOOTH_OPERATOR_IMAGE` → `SMOOTH_OPERATIVE_IMAGE`,
  `SMOOTH_OPERATOR_RUNNER` → `SMOOTH_OPERATIVE`,
  `SMOOTH_OPERATOR_RUNNER_NATIVE` → `SMOOTH_OPERATIVE_NATIVE`.
- System prompt: "You are Smooth Operator…" → "You are a Smooth operative…".
- CLI: `th operators list/kill` → `th operatives list/kill`
  (`OperatorsCommands` → `OperativesCommands`).
- bigsmooth worker types: `OperatorClient` → `OperativeClient`,
  `OperatorRegistry` → `OperativeRegistry`, `operator_client.rs` →
  `operative_client.rs`.
- Docs: `docs/Architecture/Operators.md` → `Operatives.md` (+ cross-links).

The `operator_id` value/proto field name is kept (scoped value, not the
colliding `smooth-operator` string) — no wire change. The sandbox VM name
format moved to `smooth-operative-<id>`.
