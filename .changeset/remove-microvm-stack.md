---
'@smooai/smooth': minor
'@smooai/smooth': minor
'@smooai/smooth': minor
---

Remove the microVM / sandbox stack (pearl th-f4a801).

Big Smooth used to dispatch each task into a per-task
[microsandbox](https://github.com/microsandbox/microsandbox) microVM fronted by
a per-VM access-control cast (Wonk + Goalie + Narc + Scribe). That whole stack
was deleted — git history at this PR's parent commit is the archive, and the
smooth-daemon epic (th-c89c2a) is the forward path.

**Deleted crates:** `smooth-wonk` (in-VM access authority), `smooth-goalie`
(in-VM network + FUSE proxy), `smooth-bootstrap-bill` (host-side microsandbox
broker), `smooth-host-stub` (VM credential broker), `smooth-credential-helper`.
The `microsandbox` dependency and the `direct-sandbox` feature are gone.

**Behavior changes:**

- Dispatch now always runs the operative as a host subprocess, in-process
  (`dispatch_ws_task_direct`). Narc tool surveillance still runs in-process;
  there is no longer VM isolation or Wonk/Goalie network/filesystem policy
  enforcement.
- `th up` starts Big Smooth on the host (the old `th up direct`). `th up direct`
  and the sandboxed boot path are removed.
- `th run` still dispatches via Big Smooth but its VM-only flags (`--image`,
  `--memory-mb`, `--keep-alive`) are removed.
- `th cache` (project-scoped VM build caches) is removed.
- The bigsmooth `smooth-operative` binary is no longer cross-compiled to
  `aarch64-unknown-linux-musl` / mirrored to `~/.smooth/runner-bin/`; the native
  build installed via `pnpm install:th` is what dispatch execs.

Narc, policy, scribe, archivist, and the operative's tools/agent-loop are kept.
