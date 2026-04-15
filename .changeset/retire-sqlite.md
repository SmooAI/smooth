---
"@smooai/smooth": minor
---

Retire SQLite. Pearls, sessions, memories, config, and worker metadata
all live in the Dolt store at `~/.smooth/dolt/` (home) or
`<repo>/.smooth/dolt/` (per-project). `smooth.db` is gone; the
dashboard reads "Dolt store (pearls + config)" instead of
"Database (SQLite)". `th pearls migrate-from-sqlite` removed —
transitional tool, no longer needed.
