# Build Workflow

#engineering

> [!info] Two builds matter
> Native `cargo build` for the host `th` binary, and a cross-compile of `smooth-operator-runner` to `aarch64-unknown-linux-musl` so it can run inside the microsandbox guest. `pnpm install:th` does both, plus the web bundle.

## Commands

```bash
cargo build                          # Build all crates (debug)
cargo build --release -p smooth-cli  # Release th (~10MB)
cargo test                           # Run all tests (200+ across crates)
cargo fmt                            # Format (rustfmt.toml: 160 width)
cargo clippy                         # Lint (pedantic + nursery)

pnpm install:th                      # Build web SPA + cross-compile runner + install th
pnpm build:web                       # Just rebuild the embedded Vite SPA
pnpm build:runner                    # Just cross-compile operator-runner (mirrors to ~/.smooth/runner-bin/)
```

## One-time dev setup

```bash
# Rust + cross-compile chain
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild
pip3 install ziglang                         # provides python-zig for cargo-zigbuild

# Build the in-VM runner
bash scripts/build-operator-runner.sh        # → target/aarch64-unknown-linux-musl/release/smooth-operator-runner

# Build smooth-dolt (Go binary; embedded Dolt engine)
brew install icu4c                           # macOS; required by the Dolt link
bash scripts/build-smooth-dolt.sh            # → target/release/smooth-dolt (~145MB)
```

Re-run `scripts/build-operator-runner.sh` after changing anything under `crates/smooth-operator-runner/` or its transitive deps. Re-run `build-smooth-dolt.sh` after changing the Go shim.

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

## Testing

> [!warn] Tests are mandatory
> Every crate, every module, every public function MUST have tests. `cargo test` must pass before any commit. `cargo clippy` must be clean (zero warnings). `cargo fmt -- --check` must pass.

- Unit tests colocated in each module (`#[cfg(test)]`).
- Integration tests for cross-crate flows (e.g. policy → sandbox, wonk → goalie).
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
- [[Operators]]
- [[Bench-Harness]]
