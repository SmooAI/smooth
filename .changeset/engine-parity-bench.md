---
"@smooai/smooth": minor
---

Restore the `smooth-bench` crate (Aider-Polyglot slice) and add an engine-parity axis.

The mature bench crate was deleted in the microVM/daemon rewrite. This restores
the focused Aider-Polyglot slice — the `run_aider_polyglot` single-task runner,
the curated-task sweep, the WS chat driver, scoring, and auto-approve — against
current `main` (deps now resolve to the published `smooai-smooth-operator-core`
engine plus the in-tree `smooth-cast`/`smooth-code`/`smooth-pearls`). The
SWE-bench / replay / research / cleanup / TUI-driver scorers were left out.

New engine-parity benchmark (pearl th-4c3e2d): `smooth-bench score` runs the
curated aider-polyglot suite through each of the five smooth-operator
`LocalServer` implementations — Rust, Go, TypeScript, Python, .NET — scoring per
engine (and per model).

- `--engine <rust|go|ts|python|dotnet>` (repeatable, default all) and
  `--model <id>` (repeatable, default `deepseek-v4-flash`).
- Each engine is booted the way `scripts/operator-serve.sh` does (uniform
  env contract: `SMOOAI_GATEWAY_URL/KEY`, `SMOOTH_PERSONA`, `SMOOAI_MODEL`, plus
  the per-engine bind var), the sweep runs against it over the canonical WS
  protocol, then it's torn down before the next cell.
- Per-engine×model results carry the engine + model dimensions and emit in the
  JSON-lines + summary-table format.

The matrix runner is parameterised on an `EngineBooter` trait, so the
engine×model aggregation and the engine→boot-command mapping are unit-tested
without a live LLM or real servers. A real scoring run needs `SMOOAI_GATEWAY_KEY`
on the runner.
