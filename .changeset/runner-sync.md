---
"@smooai/smooth": patch
---

build-operator-runner.sh + install:th: keep `~/.smooth/runner-bin/` in lockstep with `target/`

Big Smooth's `find_operator_runner_binary` walks up from
`CARGO_MANIFEST_DIR` looking for
`target/aarch64-unknown-linux-musl/release/smooth-operator-runner`.
A long-running `th up` daemon compiled in a worktree whose `target/`
no longer holds the binary can fall back to a stale
`~/.smooth/runner-bin/` copy left by an earlier setup. Net effect:
fresh runner code (e.g. the `coding_workflow` role gate from
`th-c1e2c0`) never reaches the sandbox even after rebuild +
reinstall — sandbox runs old binary, oracle still gets shoved
through fixer's coding workflow.

Two fixes:

- `scripts/build-operator-runner.sh` now copies the freshly-built
  binary into `~/.smooth/runner-bin/smooth-operator-runner` after
  every cross-compile. Both find paths resolve to fresh.
- New `pnpm build:runner` script wraps the build script. `pnpm
  install:th` chains `build:web && build:runner && cargo install`,
  so a single `pnpm install:th` now refreshes everything: web
  bundle, sandbox runner, and host `th` binary. The script's
  cross-compile is incremental — adds ~5s when no runner sources
  changed, ~30s when they did. Worth it to kill the stale-binary
  footgun.
