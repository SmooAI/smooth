# Direct Mode

#architecture

> [!warn] Trusted environments only
> Direct mode runs Smooth and every operative on the host with no microVM around them. No hardware isolation. Cast hooks (Wonk, Narc, Goalie) still fire, but the kernel boundary is gone. Use it inside an already-trusted environment such as a CI runner, a dedicated devbox, or a benchmark harness.

## Activating it

```bash
th up direct
```

Or, for harnesses that can't easily change argv:

```bash
SMOOTH_WORKFLOW_DIRECT=1 th up
```

The CLI flag wins over the env var when both are set.

## What changes

```
              th up direct
                 │
                 ▼
   ┌────────────────────────────────────────────────────────┐
   │  Host process (daemonised as ~/.smooth/smooth.pid)     │
   │                                                        │
   │   tokio runtime — Big Smooth                           │
   │   ├── Big Smooth      (axum :4400; gRPC bigsmooth.sock)│
   │   ├── Wonk            (gRPC wonk.sock)                 │
   │   ├── Goalie          (HTTP proxy on loopback)         │
   │   ├── Narc            (gRPC narc.sock + LLM judge)     │
   │   ├── Scribe          (gRPC scribe.sock)               │
   │   ├── Archivist       (HTTP :4401 + SSE /events)       │
   │   └── Diver           (in-process)                     │
   │                                                        │
   │   Operative(s) — native subprocess per pearl     │
   │   └── Dials the same .sock files in a host tempdir     │
   │                                                        │
   │   UDS dir: ~/.smooth/run/$XXXXX/ (tempdir per `th up`) │
   │   Filesystem: full host access (workspace = cwd)       │
   │   Network:    direct, only mediated by Goalie at app   │
   │                level (not iptables)                    │
   └────────────────────────────────────────────────────────┘
```

- **No microVM boot.** Big Smooth comes up as a daemonised child of `th`, writing stdout/stderr to `~/.smooth/smooth.log`. `th down` `kill`s the pid.
- **Same gRPC topology as sandboxed mode.** `single_process::bootstrap_from_app_state` still binds `narc.sock`, `wonk.sock`, `scribe.sock`, `bigsmooth.sock` — just under a tempdir on the host instead of `$XDG_RUNTIME_DIR/smooth/` inside a VM. The operative subprocess dials them with the same `tonic` clients. See [[Transport]].
- **Native operative binary.** `dispatch_ws_task_direct` resolves `smooth-operative` from `target/release` (or `target/debug`, or `SMOOTH_OPERATIVE_NATIVE`), not the cross-compiled musl one.
- **No bind mounts.** Workspace = host cwd. Tool sandbox root = `SMOOTH_WORKSPACE` (default the cwd).
- **No kernel-level egress.** Goalie still gets HTTP_PROXY pointed at it, but the host kernel won't reject bypasses — anything the operator does outside the agent loop is unmediated.

## When to reach for it

- Headless E2E tests where launching a microVM per run is too slow.
- The bench harness (`th bench`) when you're sweeping scores and the agent is well-trusted code.
- CI environments (GitHub Actions runners, most cloud VMs) where nested virtualization isn't available.
- Local debugging when you want to attach a debugger to the runner.

## When NOT to use it

- Anything where you're handing the agent an untrusted prompt or untrusted code.
- Anything where the agent has secrets in its env beyond what you'd give a shell script.
- Production-style usage. `th up` (sandboxed) is the default for a reason.

## How dispatch differs

`dispatch_ws_task` reads `SMOOTH_WORKFLOW_DIRECT` and forks:

| Sandboxed                          | Direct                           |
| ---------------------------------- | -------------------------------- |
| `dispatch_ws_task_sandboxed`       | `dispatch_ws_task_direct`        |
| musl runner mounted into VM at     | Native runner from `target/`     |
| `/opt/smooth/bin/`                 |                                  |
| Workspace bind-mounted at          | Workspace = host cwd             |
| `/workspace`                       |                                  |
| Per-task `create_sandbox` call     | Per-task `tokio::process::Command` |
| Cast lives in same microVM         | Cast lives in same host process  |

Both paths emit the same `AgentEvent` stream and the same WebSocket `ServerEvent` shape. The dashboard can't tell which one ran a task except by latency.

## Daemonisation details

- PID file: `~/.smooth/smooth.pid`
- Log: `~/.smooth/smooth.log` (stdout + stderr combined)
- `th up direct` checks for a live pid before forking; stale pid files are removed.
- `th down` kills the pid and removes the file.
- `th status` reads the pid file and `kill -0`s it.

## Related

- [[Sandboxed-Mode]]
- [[Dispatch]]
- [[Operations/Running-Locally]]
