---
"@smooai/smooth": patch
---

Cleanup: subagent_dispatch test, smooth-web auto-placeholder, /verbose hides per-line `[runner]` stderr too

Three small fixes:

- **subagent_dispatch test** — `fixer_role_dispatches_scout_and_only_final_summary_leaks` asserted `obj.len() == 3`, but `DispatchResult` now has 4 fields when `verified_paths` is non-empty (the trust-but-verify follow-up from C4 added that field with `skip_serializing_if = Vec::is_empty`, and the test scenario triggers a `src/` path mention). Replaced the strict count check with a positive assertion that the three required fields are present and a closed-set check that no unexpected fields appear.
- **smooth-web build.rs placeholder** — fresh worktrees previously failed to compile until you manually ran `pnpm build:web` to populate `crates/smooth-web/web/dist/index.html` (rust-embed needs the directory to exist at macro-expansion time). New `build.rs` writes a tiny placeholder if `dist/index.html` is missing, so any cargo build / cargo test in a fresh worktree just works. The first real `vite build` overwrites it. The directory is git-ignored so the placeholder doesn't leak into commits.
- **`/verbose` hides per-line `[runner]` stderr** — pearl `th-ef181a` introduced `/verbose` and hid content after the `[runner stderr]` marker. But the sandboxed dispatch path (`server.rs:2596`) forwards each runner stderr line as its own TokenDelta with prefix `[runner] ` (no separator marker), and those lines kept leaking into the assistant content even with verbose off. Render now filters lines whose first 9 chars are `[runner] `, `[runner stderr]`, or `[cast-summary]` when verbose is off. Verbose on shows everything as before.
