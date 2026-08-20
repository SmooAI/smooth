---
'@smooai/smooth': minor
---

`smoo config schema` — patch the @smooai/config schema straight from the CLI, remote-first. `show` renders every declared key per tier with type/description/default; `add` upserts a key declaration (`--tier secret|public|feature_flag|limit`, with `--type`/`--description`/`--default` and `--min`/`--max` clamp bounds for limits); `rm` removes one. Both write-verbs print the would-be change, support `--dry-run`, POST a new schema version via the same endpoint `push` uses, and keep a pulled local `.smooai-config/schema.json` in sync when one exists. Foot-guns are refused loudly: adding a key already declared in a different tier shows the existing declaration, and `rm` reminds you that values are not deleted.
