# Troubleshooting

#operations

> [!info] Known traps
> Things we've hit, what they look like, and how to get past them. Add new entries as you find them. (Entries about the removed microVM sandboxed mode were dropped 2026-07 — see [[../Decisions/ADR-004-remove-microvm-sandbox-stack]]; git history has them.)

## `smooth-operative binary not found` / dispatch closes pearls with `cost_usd=0`

Cause: the native runner hasn't been built, or it's not where Big Smooth expects.

Fix:

```bash
cargo build -p smooth-operative --release   # auto-discovered from target/release/
# or
pnpm install:th                             # installs to ~/.cargo/bin/smooth-operative
```

Or set `SMOOTH_OPERATIVE_NATIVE=/absolute/path/to/smooth-operative` to point Big Smooth at a specific binary.

## "Smooth is already running (pid N)" — but it isn't

Cause: stale `~/.smooth/smooth.pid` after a crash or `kill -9`.

Fix: `rm ~/.smooth/smooth.pid` and retry `th up`. The CLI already detects-and-removes stale pids on the next launch, but if you're scripting around the failure, this is the manual reset.

## Port 4400 already in use

Cause: another `th` is running, or another service grabbed the port.

Fix:

```bash
th down                              # stops the daemon if the pid file exists
lsof -i :4400                        # find the offending pid
```

Or pick a different port: `th up --port 4500`.

## `pearls push` / `pearls pull` complains about Dolt

Cause: `smooth-dolt` binary missing or stale.

Fix:

```bash
brew install icu4c                   # macOS
bash scripts/build-smooth-dolt.sh
```

The build produces `target/release/smooth-dolt`; `pnpm install:th` mirrors it into place.

## Tests pass locally, fail in CI

Most common: the bench harness depends on the native runner in `target/release/`. CI must build it explicitly:

```bash
cargo build -p smooth-operative --release
```

before `cargo test -p smooth-bench`.

## Related

- [[Running-Locally]]
- [[../Architecture/Dispatch]]
