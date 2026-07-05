# Running Locally

#operations

> [!info] One mode, three commands
> `th up`, `th down`, `th status`. Everything else is layered on top. (The microVM sandboxed mode was removed 2026-07 — see [[../Decisions/ADR-004-remove-microvm-sandbox-stack]].)

## Quickstart

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh

# Sign in (resolves all smooth-* model slots through Smoo AI's gateway)
th auth login smooai-gateway

# Start Smooth (daemonizes by default)
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

## How `th up` runs

`th up` boots Big Smooth directly on the host and daemonizes. Dispatched tasks exec the native `smooth-operative` binary as a host subprocess with in-process [[../Architecture/The-Cast#Narc|Narc]] tool surveillance. There is no VM, no OCI image pull, no forwarded port.

```bash
th up                                # Daemonized (default)
th up --foreground                   # Run in foreground; ctrl-C kills it
th up --port 4500                    # Use a different port
```

State written by boot:

- `~/.smooth/smooth.pid` — daemon pid
- `~/.smooth/smooth.log` — daemon stdout+stderr

Tear-down: `th down` kills the pid, removes the file.

Dispatch needs the native operative binary. `pnpm install:th` installs it to `~/.cargo/bin/smooth-operative`; auto-discovery also checks `target/{release,debug}/` relative to the repo. Override with `SMOOTH_OPERATIVE_NATIVE=/abs/path`.

## Project context files (like CLAUDE.md)

A dispatched operative reads project context files at startup and prepends them to its system prompt as a `## Project Context` block — so a fresh agent walks in knowing the repo layout instead of rediscovering it every turn (pearl th-5002c4).

Discovery stacks two layers; within a layer the first hit wins:

- **User layer** (read once): `~/.smooth/CONTEXT.md` → `~/.smooth/AGENTS.md` → `~/.smooth/CLAUDE.md` — facts you want every dispatch to know (e.g. "I run a smoo-hub dashboard at smoo-hub:8787").
- **Project layer** (walked up from the workspace): `<repo>/.smooth/CONTEXT.md` → `<repo>/SMOOTH.md` → `<repo>/AGENTS.md` → `<repo>/CLAUDE.md`.

An `AGENTS.md` / `SMOOTH.md` may list additional files under a `## File References` section (`- [Label](path.md#section) — note`); those are resolved and appended inline. Separately, `<repo>/.smooth/MEMORY.md` (the agent's own `write_memory` store) is injected as a `## Workspace Memory` block.

Each injected block is capped at **16 KB**; an oversized file is truncated on a UTF-8 boundary with a `[... truncated ...]` marker so a giant README can't blow the context budget.

## Useful knobs

| Flag / env                    | Default        | Meaning                                              |
| ----------------------------- | -------------- | ---------------------------------------------------- |
| `--port`                      | 4400           | Big Smooth API port                                  |
| `--bind`                      | 127.0.0.1      | Interface to bind on. ⚠️ The API has no auth today — anything other than loopback exposes every route to the network (pearl th-6db839) |
| `--no-leader`                 | off            | Skip starting Big Smooth (API + web UI)              |
| `--max-operators N`           | 3              | Max concurrent operatives                            |
| `--skip-test`                 | off            | Skip the workflow TEST phase (bench only)            |
| `--foreground`                | off            | Don't daemonise                                      |
| `SMOOTH_SANDBOX_MAX_CONCURRENCY` | 3           | Equivalent to `--max-operators`                      |
| `SMOOTH_OPERATIVE_NATIVE`     | auto-discovered | Absolute path to the `smooth-operative` binary      |
| `SMOOTH_WORKFLOW`             | 1              | Multi-phase workflow; `0` falls back to single-Agent |

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
- [[../Architecture/Dispatch]]
- [[../Decisions/ADR-004-remove-microvm-sandbox-stack]]
- [[Troubleshooting]]
