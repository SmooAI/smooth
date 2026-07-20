---
"@smooai/smooth": patch
---

Fix the broken dev loop and purge the microVM-era leftovers that were misleading
agents and humans reading this repo (th-e827ba).

- **`pnpm install:th` was broken — it exited 101 for everyone.** It ran `cargo
  install --path crates/smooth-operative`, a directory deleted with the microVM
  stack. `th` itself installs first, so the failure landed at the end: the
  binary looked fine while the documented dev loop (CLAUDE.md §2) returned
  non-zero, failing any hook or CI step gated on it. It now installs
  `smooth-daemon` — what `th daemon` / `th up` actually need resolvable on PATH,
  since `th` deliberately doesn't link the daemon in. Same fix for
  `install:th:full`. Verified by running to completion.
- **Deleted `scripts/bench.sh`** — a wrapper around `smooai-smooth-bench`, also
  deleted with the microVM stack.
- **`CLAUDE.md` §1/§4 rewritten against the real tree.** The workspace structure
  and "Key Crates" list documented six crates that no longer exist
  (`smooth-bigsmooth`, `smooth-narc`, `smooth-scribe`, `smooth-archivist`,
  `smooth-operative`, and `smooth-operator` — which now lives in its own repo)
  while omitting seven that do. §4's module table, dispatch description, and
  security architecture all described the deleted `smooth-bigsmooth` and are now
  written against `smooth-daemon` (permission hook → Narc → kernel sandbox).
- **`smooth-goalie` documented as it is now**: not the in-VM Wonk-delegating
  network proxy, but the daemon's egress boundary via `AuditLogger` +
  `run_proxy_local`.
- **Broken paths fixed**: `crates/smooth-cli/src/api/` (doesn't exist) →
  `src/smooai/`; `th operators` → `th operatives`; the `th cache` /
  `~/.smooth/project-cache` docs described a command that no longer exists.
- **`rusqlite` 0.32 → 0.40.** The pin existed to unify `libsqlite3-sys` with
  microsandbox's sea-orm→sqlx tree; microsandbox is gone and rusqlite is now the
  only crate linking sqlite3. No API changes needed.
- **User-facing strings corrected**: `th code` cold-start printed "starting
  Safehouse microVM" and "cast online (wonk · goalie · narc · scribe · archivist
  · diver · groove)"; boot failures blamed a "Safehouse microVM" that hasn't
  existed for weeks.
- Stale `//!` doc comments and crate manifest descriptions swept across
  `smooth-goalie`, `smooth-tools`, `smooth-policy`, `smooth-diver`,
  `smooth-daemon`, and `smooth-cli`, plus the README workspace tree and the dead
  microsandbox image / project-cache sections.
