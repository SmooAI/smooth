---
'@smooai/smooth': minor
---

SMOODEV-4260: `th chrome browse groups` lists every Chrome tab group, Claude in Chrome's included, with its tab count and when it was last used. `groups close` and `groups ungroup` clean them up by id, `--title` glob, `--idle 2h` or `--all`, with `--dry-run`. A group holding a window's active tab is never touched. Also `groups rename`, `color`, `collapse` and `expand`, and a `browse_groups` MCP tool. The extension is now 0.2.0: run `th chrome install` and reload it.
