---
'@smooai/smooth': minor
---

`th ci-queue shim install | uninstall | status` writes PATH shims for `cargo`, `xcodebuild` and `gradle` (SMOODEV-3355). They go into `~/.local/bin` by default, ahead of the real tools, so heavy runs from any caller go through the machine-wide queue, agents included. A shim runs the tool directly in four cases: inside an already-queued job (the recursion guard, so cargo calling cargo never deadlocks on its own parent), with `CI_QUEUE=off`, when `th` is missing, and for light commands (`cargo --version`, `metadata`, `fmt`, any `--help`, …). Install is idempotent and never overwrites a file it did not write unless you pass `--force`. Uninstall restores exactly the prior state. No shell rc file is edited. `th ci-queue run -- cargo <light command>` no longer takes the cargo lock, so `cargo --version` stops waiting behind someone else's build.
