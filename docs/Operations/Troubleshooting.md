# Troubleshooting

#operations

> [!info] Known traps
> Things we've hit, what they look like, and how to get past them. Add new entries as you find them.

## `th up` hangs at "booting boardroom microVM"

Cause: the microsandbox daemon hasn't pulled the image yet, or your internet is slow.

Fix: wait. First pull of `ghcr.io/smooai/boardroom:latest` can take a minute. After the first pull it's cached locally.

Check progress with `microsandbox` SDK logs (look at the launching `th` process's stderr).

## `smooth-operator-runner binary not found`

Cause: the cross-compiled runner hasn't been built, or it's not where Big Smooth expects.

Fix:

```bash
bash scripts/build-operator-runner.sh
pnpm install:th                # mirrors the runner into ~/.smooth/runner-bin/
```

Or set `SMOOTH_OPERATOR_RUNNER=/absolute/path/to/smooth-operator-runner` to point Big Smooth at a specific binary.

For direct mode: the dispatch path uses the **native** runner instead. Build with `cargo build -p smooth-operator-runner --release`, or set `SMOOTH_OPERATOR_RUNNER_NATIVE=/path`.

## "Smooth is already running (pid N)" — but it isn't

Cause: stale `~/.smooth/smooth.pid` after a crash or `kill -9`.

Fix: `rm ~/.smooth/smooth.pid` and retry `th up direct`. The CLI already detects-and-removes stale pids on the next launch, but if you're scripting around the failure, this is the manual reset.

## Sandboxed-mode operator dispatch fails with `create_sandbox failed`

Cause: the inside-VM `microsandbox` SDK can't spawn nested microVMs (no nested virt on Apple HVF).

> [!todo] Known transition gap
> Operator dispatch from inside the Boardroom VM is the in-progress half of the consolidation. While this is being resolved, run end-to-end loops in direct mode:
>
> ```bash
> th down
> th up direct
> ```

## Port 4400 already in use

Cause: another `th` is running, or another service grabbed the port.

Fix:

```bash
th down                              # stops both sandboxed and direct flavors if state files exist
lsof -i :4400                        # find the offending pid
```

Or pick a different port: `th up --port 4500`.

## `SMOOTH_NARC_URL` set to 127.0.0.1, host_tool fails

Cause: inside the microVM, `127.0.0.1` is the guest's own loopback, not the host. `detect_routable_host_ip` should have picked a real interface IP but fell through.

Fix: set `SMOOTH_NARC_URL=http://<host-interface-ip>:4400` explicitly before `th up`. Look up the host IP with `ipconfig getifaddr en0` (macOS) or `hostname -I | awk '{print $1}'` (Linux).

## `pearls push` / `pearls pull` complains about Dolt

Cause: `smooth-dolt` binary missing or stale.

Fix:

```bash
brew install icu4c                   # macOS
bash scripts/build-smooth-dolt.sh
```

The build produces `target/release/smooth-dolt`; `pnpm install:th` mirrors it to `~/.smooth/runner-bin/`.

## Tests pass locally, fail in CI

Most common: the bench harness depends on the native runner in `target/release/`. CI must build it explicitly:

```bash
cargo build -p smooth-operator-runner --release
```

before `cargo test -p smooth-bench`.

## Related

- [[Running-Locally]]
- [[../Architecture/Sandboxed-Mode]]
- [[../Architecture/Direct-Mode]]
