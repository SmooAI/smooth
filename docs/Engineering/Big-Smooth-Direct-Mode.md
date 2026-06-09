# Big Smooth — Direct (host) vs Sandboxed (safehouse VM) mode

Big Smooth runs in one of two modes. Choose at `th up` time.

| | `th up` (sandboxed, default) | `th up direct` |
|---|---|---|
| **Boot time** | ~30s — boots a safehouse microVM + the in-VM cast | **~0.3s** — daemon starts directly on the host |
| **Isolation** | Strong — agent runs inside a microVM, safehouse mediates filesystem/network | None — agent is a host subprocess; tools execute against the host filesystem |
| **When to use** | Untrusted code, agent dispatches you don't fully control, CI runners that need defense in depth | Pre-trusted environments — dedicated devbox, CI runner you own, bench harnesses |
| **Idle timeout default** | 24 h (was 30 min — pearl `th-1b9b3e`) | 24 h |
| **Native runner needed?** | No — runner is baked into the safehouse OCI image | **Yes** — build with `cargo build --release -p smooai-smooth-operative` and either auto-discovery picks it up from `~/.cargo/shared-target/release/smooth-operative`, or you set `SMOOTH_OPERATIVE_NATIVE=/abs/path/to/runner` before `th up direct` |

## Why this matters for parity with pi + opencode

Pi (`@earendil-works/pi-coding-agent`) and OpenCode (`opencode`) both boot in ~3s
and have no daemon model. Smooth's sandboxed default looked like a "30s boot,
sometimes crashes" agent against them. Direct mode is a near-100× boot speedup
that brings smooth into the same launch-time class as pi + opencode for
pre-trusted use cases (dev machines, bench harnesses).

## Smoke test

```bash
# Build the native runner once per checkout.
cargo build --release -p smooai-smooth-operative

# Start in direct mode.
th down
SMOOTH_OPERATIVE_NATIVE=~/.cargo/shared-target/release/smooth-operative \
  th up direct

# Confirm: th status should report healthy in under a second.
th status
```

The runner-bin auto-discovery has a paper-cut tracked under pearl `th-e74aa6`:
when the env var is unset the error message names the build command but doesn't
mention that auto-discovery from `~/.cargo/shared-target/release/` will work if
you've built it. Either approach gets you there.

## Bench harness usage

`smooth-bench` doesn't care which mode Big Smooth is in — both expose the same
HTTP API at `localhost:4400`. The `SmoothDriver` in
`crates/smooth-bench/src/agent_driver.rs` just spawns `th code` against the
running daemon. So:

```bash
# Sandboxed mode (default — slow boot, more isolation)
th up
cargo run -p smooai-smooth-bench -- score-cleanup --driver=smooth …

# Direct mode (fast boot, host trust)
th down
SMOOTH_OPERATIVE_NATIVE=~/.cargo/shared-target/release/smooth-operative th up direct
cargo run -p smooai-smooth-bench -- score-cleanup --driver=smooth …
```

Result JSON includes `dispatch="direct"` or `dispatch="sandboxed"` in the daemon
log (`~/.smooth/log/th.log`) so post-hoc you can tell which mode each result
came from.

## Recent bench numbers

`deepseek-v4-flash` via `llm.smoo.ai`, strict coach, 4 cleanup fixtures
(`cleanup-impossible-task`, `cleanup-pycache-debris`, `cleanup-disk-bloat`,
`cleanup-node-modules-orphans`):

| backend | aggregate | boot time | notes |
|---|---|---|---|
| mock | 1.000 | n/a | bash baseline |
| **pi** | **1.000** | ~3s | new reference high-water |
| opencode | ≥0.93 | ~3s | reliable on tested fixtures |
| **smooth-direct** | **0.850** | **~0.3s** | beats sandboxed; matches pi boot time |
| smooth-sandboxed | 0.789 | ~30s | run-to-run variance still present |

Pearls related: `th-0fc29f` (boot time, this doc closes it),
`th-1b9b3e` (idle timeout, closed), `th-6e361d` (pycache variance, open).
