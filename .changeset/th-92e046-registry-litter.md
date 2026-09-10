---
'@smooai/smooth': patch
---

`th pearls projects` no longer fills up with hook litter. Opening the pearl store used to register whatever directory it was opened from, and `th prime` (the Codex SessionStart hook) opens it from any cwd — so `~/.smooth/registry.json` collected Codex scratch dirs, `$TMPDIR`, `$HOME`, even `/`. A plain open now registers the project only when its root is a git repository other than `/` or `$HOME`; `th pearls init` registers any directory explicitly and that entry survives. Every open also prunes implicit entries that are not git repos (alongside the existing dead-path prune), so an existing registry heals on the next `th pearls` call. (th-92e046)
