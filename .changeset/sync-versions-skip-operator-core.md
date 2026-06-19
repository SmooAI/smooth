---
"@smooai/smooth": patch
---

build: `sync-versions.mjs` no longer bumps the external `operator-core` dep

`scripts/sync-versions.mjs` rewrote the `version = "…"` on every
`smooth-X = { … }` workspace.dependencies line to the workspace version —
including `smooth-operator = { …, package = "smooai-smooth-operator-core" }`,
the **external** agent engine published from its own repo. When the version PR
bumped the workspace to 0.14.1, it rewrote the operator-core requirement to
`^0.14.1`, which doesn't exist on crates.io (latest is 0.14.0), breaking
`cargo build --examples --workspace` and the version PR's checks. The script
now skips any workspace-dependency line that pins `smooai-smooth-operator-core`,
leaving its requirement at the real published version. Pearl th-1ee32b.
