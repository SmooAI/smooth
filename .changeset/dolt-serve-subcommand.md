---
"@smooai/smooth": minor
---

`smooth-dolt`: add a long-running `serve` subcommand. Opens the embedded Dolt database once and accepts JSON-line requests over a Unix domain socket — eliminates the per-call subprocess spawn that was hanging Big Smooth's `/api/projects` handler on smoo-hub (see pearl th-1a61a7). Existing one-shot subcommands (`init`, `sql`, `commit`, `log`, `push`, `pull`, etc.) are unchanged so the CLI keeps working. Phase A of pearl th-1ff010 — a Rust client and Big Smooth integration land in subsequent commits.
