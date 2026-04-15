---
"@smooai/smooth": patch
---

`th doctor --init-home-repo` scaffolds `~/.smooth/` as a git repo for
backup / cross-machine sync. Writes a `.gitignore` that excludes
secrets (`providers.json`), service logs, audit logs, the Dolt
store (has its own push/pull), the project cache, and ephemeral
debug captures. Idempotent. Optional `--remote <url>` adds origin.
