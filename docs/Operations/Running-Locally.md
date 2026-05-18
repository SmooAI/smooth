# Running Locally

#operations

> [!info] Two modes, four commands
> `th up`, `th up direct`, `th down`, `th status`. Everything else is layered on top.

## Quickstart

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh

# Sign in (resolves all smooth-* model slots through Smoo AI's gateway)
th auth login smooai-gateway

# Start Smooth (sandboxed by default)
th up

# Open the embedded web UI in your browser
open http://localhost:4400

# Or attach the TUI
th code
```

Then stop:

```bash
th down
```

## Sandboxed mode (default)

```bash
th up                                # Boots the Safehouse microVM
th up --port 4500                    # Use a different forwarded port
SMOOTH_SAFEHOUSE_IMAGE=ghcr.io/smooai/safehouse:dev th up
                                     # Override the OCI image
```

State written by sandboxed boot:

- `~/.smooth/sandboxed.vm` — the microsandbox VM name (so `th down` finds it)

Tear-down: `th down` reads `sandboxed.vm`, calls `microsandbox::destroy_sandbox`, removes the file.

See [[../Architecture/Sandboxed-Mode]] for what's inside.

## Direct mode (escape hatch)

```bash
th up direct                         # Daemonised, no sandbox
th up direct --foreground           # Run in foreground; ctrl-C kills it
SMOOTH_WORKFLOW_DIRECT=1 th up       # Env-var override; for harnesses that can't pass argv
```

State written by direct boot:

- `~/.smooth/smooth.pid` — daemon pid
- `~/.smooth/smooth.log` — daemon stdout+stderr

Tear-down: `th down` kills the pid, removes the file.

See [[../Architecture/Direct-Mode]] for what changes.

## Useful knobs

| Flag / env                    | Default        | Meaning                                              |
| ----------------------------- | -------------- | ---------------------------------------------------- |
| `--port`                      | 4400           | Big Smooth API port (forwarded out of the VM)        |
| `--no-leader`                 | off            | Skip access-leader-election bootstrap                |
| `--max-operators N`           | 3              | Sandbox-pool concurrency cap                         |
| `--skip-test`                 | off            | Pass-through to runner: skip TEST phase (bench only) |
| `--foreground`                | off            | Don't daemonise (direct mode only)                   |
| `SMOOTH_SAFEHOUSE_IMAGE`      | `…/safehouse:latest` | OCI image for the safehouse VM                  |
| `SMOOTH_SANDBOX_MAX_CONCURRENCY` | 3           | Equivalent to `--max-operators`                      |
| `SMOOTH_WORKFLOW_DIRECT`      | unset          | Force direct mode (for harnesses)                    |
| `SMOOTH_WORKFLOW`             | 1              | Multi-phase workflow; `0` falls back to single-Agent |
| `SMOOTH_USE_VOLUMES`          | 1              | `0` → bind-mount project cache instead of named volume |

## Status & health

```bash
th status                            # "running (pid 12345)" or "stopped"
th doctor                            # Preflight environment checks
th doctor --init-home-repo           # Make ~/.smooth a git repo (audit history)
```

## Talking to it

| Surface         | Endpoint                                                                |
| --------------- | ----------------------------------------------------------------------- |
| Web UI          | `http://localhost:4400`                                                 |
| WebSocket       | `ws://localhost:4400/ws`                                                |
| REST            | `http://localhost:4400/api/*`                                           |
| TUI             | `th code`                                                               |
| Pearls          | `th pearls list`, `th pearls show <id>`, …                              |
| Inbox (access)  | `th inbox`                                                              |

## Related

- [[../Start-Here/What-Is-Smooth]]
- [[../Architecture/Sandboxed-Mode]]
- [[../Architecture/Direct-Mode]]
- [[Troubleshooting]]
