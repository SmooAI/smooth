---
'@smooai/smooth': minor
---

`smoo config schema pull` — bring the LOCAL schema representation up to date with the org's remote schema, correctly for both consumer kinds. A plain pulled `schema.json` (no `config.ts`) is overwritten like `smoo config pull --force`; a TypeScript consumer is never rewritten wholesale — remote keys missing locally are emitted as ready-to-paste snippets in the file's own conventions (`BooleanSchema`/`StringSchema`/`NumberSchema`, `defineLimit({ default, min, max, step })`), with `--write` appending them into the right tier block mechanically (all-or-nothing, refusing on an ambiguous block). Local-only keys are reported (push would add them) and never deleted; tier and type drift on shared keys are reported as tables. `--dry-run` prints everything and writes nothing; `--json` emits the structured report.
