---
'@smooai/smooth': patch
---

`th harness enable claude-code` now refreshes the `smooth` marketplace before updating, and updates the smooth-agent plugin's user-scope install explicitly (`--scope user`, run from `$HOME`). Previously, run inside a checkout with a project-scope pin, it bumped only that pin and left every other directory on the old plugin, and without a refresh it could "update" to a stale version. SMOODEV-3800.
