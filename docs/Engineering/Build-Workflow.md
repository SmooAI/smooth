# Build Workflow

#engineering

> [!info] All native
> Everything is a plain native `cargo build` — the `th` binary and the `smooth-operative` runner. The musl cross-compile died with the microVM stack ([[../Decisions/ADR-004-remove-microvm-sandbox-stack]]). `pnpm install:th` builds the web bundle and cargo-installs both binaries.

## Commands

```bash
cargo build                          # Build all crates (debug)
cargo build --release -p smooth-cli  # Release th (~10MB)
cargo test                           # Run all tests (200+ across crates)
cargo fmt                            # Format (rustfmt.toml: 160 width)
cargo clippy                         # Lint (pedantic + nursery)

pnpm install:th                      # Build web SPA + install th + smooth-operative to ~/.cargo/bin
pnpm build:web                       # Just rebuild the embedded Vite SPA
cargo build -p smooth-operative --release   # Just the runner (auto-discovered from target/release/)
```

## One-time dev setup

```bash
# Build smooth-dolt (Go binary; embedded Dolt engine)
brew install icu4c                           # macOS; required by the Dolt link
bash scripts/build-smooth-dolt.sh            # → target/release/smooth-dolt (~145MB)
```

Re-run `build-smooth-dolt.sh` after changing the Go shim.

## The web SPA

`crates/smooth-web/web/` is the React + Vite source. `rust-embed` includes the compiled `dist/` into the `smooth-web` crate so the embedded server serves it out of the binary.

```bash
cd crates/smooth-web/web
pnpm install
pnpm dev                             # Vite dev server at :3100 (live reload against running th up)
pnpm build                           # Build dist/, then re-`cargo build` to embed
```

## Style + lint

- **Rust:** edition 2021, max line width 160, field init shorthand, `unsafe_code = "forbid"`, `unused_must_use = "deny"`, clippy pedantic + nursery as warnings, `anyhow` for app errors / `thiserror` for library errors, `tracing` for logging.
- **TypeScript:** oxfmt for formatting, oxlint for linting, Vite + React 19 + Tailwind 4.
- **Everything else (md / json / yaml / toml / css):** `oxfmt` too — it is the repo's single formatter. `pnpm format` to write, `pnpm format:check` to verify. The `format` job in `pr-checks.yml` runs it ungated on every PR, so markdown and changesets are covered. Generated/vendored files are excluded in `.oxfmtrc.json` `ignorePatterns` (obsidian vault internals, bench session dumps, the `openapi.json` snapshot, lockfiles, and `CHANGELOG.md` — `changeset version` generates it, so formatting it would red every release PR).

## Testing

> [!warn] Tests are mandatory
> Every crate, every module, every public function MUST have tests. `cargo test` must pass before any commit. `cargo clippy` must be clean (zero warnings). `cargo fmt -- --check` must pass.

- Unit tests colocated in each module (`#[cfg(test)]`).
- Integration tests for cross-crate flows (e.g. dispatch → operative, narc hooks).
- Security-critical code (policy enforcement, secret detection, write guard) gets exhaustive coverage including adversarial inputs.
- When fixing a bug, add a regression test that fails without the fix.

## Release & versioning

`package.json` is the source of truth for the workspace version. `scripts/sync-versions.mjs` propagates it to `Cargo.toml` `workspace.package.version` and `Cargo.lock`. The Changesets workflow on GitHub Actions builds multi-platform release binaries and publishes them.

Every landable PR needs a changeset:

```bash
pnpm changeset
```

## Related

- [[Architecture-Overview]]
- [[Operatives]]
- [[Bench-Harness]]
