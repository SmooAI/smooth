---
"@smooai/smooth": patch
---

install:th now drops smooth-dolt and native smooth-operative into ~/.cargo/bin (pearl th-92dac3)

`th code` invoked from outside the smooth repo (e.g. `~/dev/smooai/smooai/`)
warned "smooth-dolt binary not found" and hard-errored on the
first dispatch with "native smooth-operative not found". Root cause
in both: the discovery code walks from `CARGO_MANIFEST_DIR` and the
process cwd looking for `target/release/<binary>` — neither finds
the binary when the cwd is a different repo.

Fix: `pnpm install:th` now also:

- Runs `cargo install --path crates/smooth-operative --force`, which
  drops the native binary at `$CARGO_INSTALL_ROOT/bin/smooth-operative`
  (typically `~/.cargo/bin/`) alongside `th`.
- Runs `scripts/install-smooth-dolt-to-cargo-bin.sh`, which copies
  `target/release/smooth-dolt` to the same dir. The copy is skipped
  when the destination is already byte-identical (cheap hot
  reinstalls; safe against a running `th up`).
- `find_native_operative_binary()` in `smooth-bigsmooth` is
  extended with a `$CARGO_INSTALL_ROOT/bin` → `$CARGO_HOME/bin` →
  `~/.cargo/bin` fallback, refactored into a pure helper
  (`cargo_bin_native_operative`) with 4 unit tests covering all
  three precedence rungs and the missing-binary case.

`install:th:full` carries the same install steps (and bumps the
unconditional smooth-dolt rebuild). `th code` from anywhere now
finds both binaries without further setup.
