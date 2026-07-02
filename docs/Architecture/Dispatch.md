# Dispatch

#architecture

> [!arch] Chat → pearl → operative subprocess → events → done
> A task enters Big Smooth over WebSocket, becomes a pearl via [[The-Cast#Diver|Diver]], is handed to a freshly spawned `smooth-operative` subprocess, and streams `AgentEvent`s back as `ServerEvent`s. Dispatch is always in-process now — one host subprocess per task, no VM fork.

## End-to-end flow

```
   User (browser / th code / API client)
     │
     │  WebSocket: { type: "TaskStart", message, model?, agent? }
     ▼
   Big Smooth (axum, :4400)
     │
     │  resolve pearl_id (caller-supplied | Diver.dispatch | PearlStore.create)
     │  mark pearl status = in_progress
     │  register teammate (UI sidebar)
     ▼
   dispatch_ws_task → dispatch_ws_task_direct
     │
     │  resolve the native smooth-operative binary
     │  spawn it as a host subprocess (cwd = workspace)
     │  pass task, model, api creds, agent role via env
     ▼
   smooth-operative (subprocess)
     │  run the agent loop (smooth-operator engine)
     │  emit AgentEvent JSON-lines on stdout
     ▼
   Big Smooth parses each line, re-emits as ServerEvent over the WebSocket
     │
     ▼
   TUI + web UI render tokens, tool calls, results, cost, completion
```

## Resolving the operative binary

`dispatch_ws_task_direct` needs a native `smooth-operative` on the host. It looks, in order:

1. `$SMOOTH_OPERATIVE_NATIVE` — an explicit absolute path.
2. `target/release/smooth-operative`, then `target/debug/smooth-operative` under the workspace root.
3. `$CARGO_HOME/bin/smooth-operative` (from `cargo install --path crates/smooth-operative`).

If none resolve, Big Smooth logs a loud startup warning and every dispatch closes its pearl with `cost_usd=0` until it's fixed. Build it with `cargo build -p smooai-smooth-operative --release` (or `--debug`). There is no longer a cross-compile / musl / image step — the operative runs on the host triple.

## What the operative gets

The subprocess inherits **host-level access** — the workspace is the caller's working directory (no bind mount, no isolation). It receives via env:

- `SMOOTH_TASK` (or `SMOOTH_TASK_FILE` for long messages) — the task.
- `SMOOTH_API_URL` / `SMOOTH_API_KEY` — the LLM gateway endpoint + key.
- `SMOOTH_MODEL` — routed model (default `gpt-5.4-mini`); the operative switches to native Anthropic request shape for Claude-class models.
- `SMOOTH_AGENT` — the agent role (default `fixer`, the full-tool coding role) which selects the tool surface.
- `SMOOTH_BUDGET_USD` / `SMOOTH_MAX_ITERATIONS` — dispatch limits.

Inside, the operative installs `PermissionHook` (role-scoped tool gating from the engine) and `NarcHook` (surveillance) on its tool registry, then runs `Agent::run_with_channel`. See [[Operatives]] and [[Security-Model]].

## Events on the wire

The operative's stdout is a stream of JSON-lines `AgentEvent`s (token deltas, tool calls, tool results, cost, completion). Big Smooth reads them line by line and forwards each as a `ServerEvent` over the WebSocket. Any non-JSON line on stdout is dropped as defense-in-depth — the operative routes all diagnostics to stderr (`tracing`).

## The `th run` shortcut

`th run <pearl>` dispatches a pearl through Big Smooth's HTTP API without opening the TUI — the same dispatch path, headless. The old VM flags (`--image`, `--memory-mb`, `--keep-alive`) were removed with the sandbox stack.

## Related

- [[Architecture-Overview]]
- [[Operatives]]
- [[The-Cast]]
- [[Security-Model]]
