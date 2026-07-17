---
"@smooai/smooth": minor
---

Big Smooth file-search tools: never hang on a large / non-git workspace tree.
The daemon can run with a broad workspace root (e.g. `~/dev` or `$HOME`), where
nothing is `.gitignore`-pruned and a naive walk descends into `~/Library`,
`~/.cargo`, `~/.rustup`, every nested repo's `node_modules`, etc. — pinning the
CPU for minutes.

- **Cross-tree directory prune** (`grep`, `list_files`): `pruned_walk`'s
  `filter_entry` name-prune now also covers the `$HOME`-level killers —
  `dist`, `build`, `.cargo`, `.rustup`, `.cache`, `.npm`, `.pnpm-store`,
  `Library`, `.Trash` — so heavy subtrees are skipped even with no `.git`.
  `.gitignore`/`.ignore` handling stays on for in-repo searches.
- **`grep` scan budget + deadline**: bounds total work at 100k entries examined
  or 10s wall-clock, returning partial results with a "search stopped early —
  narrow the path or pattern" note instead of walking a whole home dir on a
  zero-match pattern. (`list_files` already had a 50k-entry budget.)

Normal in-repo searches are unchanged — the guards only bite pathological trees.
