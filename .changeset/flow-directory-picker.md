---
'@smooai/smooth': patch
---

SmoothFlow's New Session sheet has a real **Directory** field (th-145e6b). Before, the directory came from the focused session, and the only way to change it was a free-text path hidden under "Override context". Now you can type to search every git repo and worktree under `~` (↑/↓ and Return to pick), type or paste a path, or use **Browse…** to open a folder panel. Picking re-infers the pearl, branch and title for that directory. The daemon builds the index with the `ignore` crate's parallel walker (the engine inside `fd`), and stores it in flow.db's new `repos` table. The walk stops at repo roots and skips hidden folders, `node_modules`/`target`, `~/Library` and cloud-synced folders. The index refreshes in the background and is served by `GET /api/flow/repos?q=`, with the fleet's own checkouts ranked first.
