---
"@smooai/smooth": patch
---

`th` now compiles and tests on Windows, and CI enforces it (pearl th-a165b4).

The blocker list turned out to be far shorter than pearl th-a165b4 assumed. That
pearl predates the 2026-07 microVM removal (th-f4a801) and described `smooth-cli`
as unconditionally pulling in `smooth-bigsmooth` (microsandbox → `kvm_bindings`)
and `smooth-code` (ratatui). `smooth-bigsmooth` is gone from the dependency tree
entirely, and ratatui/crossterm are cross-platform — so the `desktop` /
`cli-windows` feature-gate split the pearl designed is **not needed**, and no
feature gates were added. The default feature set is unchanged on macOS/Linux.

What actually failed to compile, and the fixes:

- `smooth-pearls`: `dolt_server.rs` is built on `std::os::unix::net::UnixStream`.
  The module is now `#[cfg(unix)]`, with a `dolt_server_stub.rs` standing in
  elsewhere. Its `try_attach` returns `None`, so `SmoothDolt::new` falls through
  to per-call CLI mode — the identical path Unix takes when no server is running.
  Keeping the type surface intact means `dolt.rs`, `store.rs`, and the CLI needed
  no changes at all.
- `smooth-cli/hooks.rs`: `PermissionsExt::from_mode` chmod'ing installed git
  hooks. Now `#[cfg(unix)]` — Git for Windows ignores the exec bit.
- `smooth-cli/claude/mod.rs`: `CommandExt::exec` to hand the terminal to tmux.
  Unix still `exec`s; other targets run tmux as a child and propagate its status.
- `smooth-cli/service.rs`: the pre-existing `#[cfg(target_os = "windows")]` Task
  Scheduler module imported `PathBuf`/`LABEL` it never used, which are themselves
  Unix-only now.
- `smooth-cast/coding_workflow.rs`: unused `target` binding in the non-Unix
  symlink fallback.

CI: the `rust` job in `pr-checks.yml` is now a `fail-fast: false` matrix over
`ubuntu-latest` + `windows-latest`, running `cargo nextest` on both. Linux
remains the canonical gate and alone runs fmt, clippy, the release build, and
test-report publishing — those are platform-independent, so duplicating them
only burns Windows minutes (billed at 2x). Windows installs `protoc` via
`arduino/setup-protoc` in place of the Linux apt step.
