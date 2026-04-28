---
"@smooai/smooth": patch
---

Pearls in project repos sync via git

`.dolt/` was globally gitignored, which meant project pearl boards
(`<repo>/.smooth/dolt/.dolt/`) were excluded too — no cross-machine
sync. Anchored the ignore to the repo root so legacy top-level
`.dolt/` stores still stay out, and added `.smooth/dolt/.gitignore`
that scopes runtime files (LOCK, temptf/, stats/) inside the pearl
store while letting the manifest + content-addressed blobs ride
along with the project's git history.

Workflow: `th pearls create` → blobs written → `git add .smooth/dolt`
→ `git commit` → `git push`. Other machine: `git pull` and
`th pearls list` shows the same board.

Trade-off: dolt blobs grow git history. Acceptable for personal +
small-team boards; revisit if a board's churn becomes painful.

Long-term: a real Dolt remote (DoltHub or self-hosted SQL server
on tailnet) is a cleaner solution; tracked in `th-94f6b6`. This
gitignore fix is the immediate "pearls sync between machines now"
unblocker.
