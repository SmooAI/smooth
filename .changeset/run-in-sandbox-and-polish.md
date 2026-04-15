---
"@smooai/smooth": minor
---

Run in sandbox — the agent does its work in a microVM, you review it live.

- `smooai/smooth-operator` image (unified — agent installs toolchains at
  runtime via `mise`; covers node/python/rust/go/bun/deno/~140 more).
- `th run [pearl-id] [--keep-alive] [--image ...] [--memory-mb N]` —
  dispatches via `/api/tasks`, streams SSE events, optionally keeps the
  VM alive for dev-server review.
- `th operators list / kill <id>` — see and tear down running VMs.
- `th cache list / prune / path / clear` — project-scoped sandbox
  cache at `~/.smooth/project-cache/<name>-<hash>/`, bind-mounted
  at `/opt/smooth/cache` inside the VM. LRU prune by mtime.
- Auto-forward common dev-server ports (3000, 3001, 4000, 4200, 5000,
  5173, 8000, 8080, 8888) on keep-alive runs; print reachable
  `http://localhost:<host>` URLs after the agent completes.
- Per-run memory override threaded through
  `TaskRequest → SandboxConfig`.
