---
"@smooai/smooth": patch
---

scripts: `pnpm install:th` now builds the embedded web SPA first

`th` embeds `crates/smooth-web/web/dist/` at compile time via
rust-embed. The old `install:th` was just `cargo install --path
crates/smooth-cli --force` — if you forgot to run `pnpm build` in
`crates/smooth-web/web` first, the new binary would silently ship
a stale web bundle. Bitten by this twice in one session.

Fix:

- New `pnpm build:web` — `pnpm install` + `vite build` inside
  `crates/smooth-web/web`, runnable on its own.
- `pnpm install:th` now chains `pnpm build:web && cargo install
  ...`. Adds ~2 seconds per install (vite build is fast); pays
  for itself the first time it prevents stale-bundle confusion.
