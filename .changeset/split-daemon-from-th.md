---
'smooai-smooth-cli': minor
'smooai-smooth-daemon': minor
---

EPIC th-c89c2a: the operator runtime is no longer statically linked into `th`.
`th daemon …` is now a thin **passthrough** (`daemon_launcher`) that resolves +
spawns a standalone `smooth-daemon` binary — found via `SMOOTH_DAEMON_BIN` /
`~/.smooth/bin` / next-to-`th` / `PATH` / the dev workspace, or **downloaded on
demand** from the GitHub release. So installing `th` (the official Smoo AI CLI)
no longer pulls in axum + the engine + adapters + the embedded widget bundle, and
`th` stops path-depping the operator crates.

- `smooth-daemon` binary gains the full daemon CLI (`run` / `operator` / `status`
  / `audit` / `schedule`); the client/format handlers moved out of `th`.
- `th` drops the `smooth-daemon` dependency; `Daemon` becomes a
  `trailing_var_arg` passthrough. `pnpm install:th` also installs the
  `smooth-daemon` binary for dev.
- Verified: `smooth-operator-server` is gone from `th`'s dep tree; `th daemon
  operator` spawns the binary, which serves the operator (widget + `/ws`) live.
