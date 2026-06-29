---
'@smooai/smooth': patch
---

Add `th config sync` and `th config build`.

- **`th config sync`** — first-class reconcile between local `.smooai-config/schema.json` and the org's remote schema. Bare `th config sync` prints the diff and tells you which direction to apply (changes nothing); `--push` applies local→remote, `--pull` applies remote→local (mutually exclusive); `--dry-run` forces diff-only. It delegates to the existing `diff`/`push`/`pull` paths — no duplicated HTTP logic, no magic two-way merge. Honors `--org-id`, `--schema-name`, `--json`, `--m2m`.
- **`th config build`** — generate `.smooai-config/schema.json` from the consumer's `config.ts` (closes the gap behind pearl th-4d1d6c). Shells out to `tsx` to import the TypeScript config and read the schema fields the `@smooai/config` runtime exposes (`PublicConfigKeys`/`SecretConfigKeys`/`FeatureFlagKeys`/`serializedAllConfigSchemaJsonSchema`), then writes the flat wire format. `--stdout` prints instead of writing; `--check` (CI parity) regenerates in memory and exits non-zero if the committed `schema.json` differs. Requires the consumer to have `@smooai/config` + `tsx`. The `push` "no schema.json" error now also points to `th config build`.
