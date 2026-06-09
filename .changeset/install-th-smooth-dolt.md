---
"smooai-smooth": patch
---

install:th now builds smooth-dolt automatically (pearl th-a49716)

Previously `pnpm install:th` ran `build:web` + `build:runner` + `cargo
install`, but never built the `smooth-dolt` Go binary that
`th pearls` needs. Fresh installs (and post-rebase ones) hit the
"⚠ smooth-dolt binary not found. Pearl sync may not work." warning
on every `th code` launch and the user had to read the warning and
run `scripts/build-smooth-dolt.sh` by hand.

Now:

- `pnpm install:th` — adds `build:smooth-dolt:if-stale`. The build
  script accepts a new `--if-stale` flag that skips the Go build
  entirely when `target/release/smooth-dolt` already exists AND
  every `*.go` / `go.mod` / `go.sum` under `go/smooth-dolt/` is
  older than the binary. Hot installs pay zero cost; cold installs
  and source bumps trigger a real build.
- `pnpm install:th:full` — NEW. Same shape as `install:th` but
  invokes `build:smooth-dolt` unconditionally. Use after a Go
  toolchain change, a Dolt upstream bump, or when you suspect a
  stale binary.

No behavior change to the standalone `bash scripts/build-smooth-dolt.sh`
invocation — without `--if-stale` it always builds, same as before.
