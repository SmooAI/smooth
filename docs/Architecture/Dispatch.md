# Dispatch

#architecture

> [!arch] Chat → pearl → operator → events → done
> A task enters Big Smooth over WebSocket, becomes a pearl via [[The-Cast#Diver|Diver]], gets handed to an operator runner, and streams `AgentEvent`s back as `ServerEvent`s. The fork between direct and sandboxed dispatch is invisible above the WebSocket.

## End-to-end flow

```
   User (browser / th code / API client)
     │
     │  WebSocket: { type: "TaskStart", message, model?, agent? }
     ▼
   Big Smooth (axum)
     │
     │  resolve pearl_id (caller-supplied | Diver.dispatch | PearlStore.create)
     │  mark pearl status = in_progress
     │  register teammate (UI sidebar)
     ▼
   dispatch_ws_task
     │
     ├── SMOOTH_WORKFLOW_DIRECT=1 ─► dispatch_ws_task_direct
     │                                  │
     │                                  ▼
     │                          spawn smooth-operator-runner
     │                          as a host subprocess
     │
     └── otherwise              ─► dispatch_ws_task_sandboxed
                                       │
                                       ▼
                               build SandboxConfig, mount runner +
                               workspace, create_sandbox, exec runner
                               in VM

   Operator runner
     │
     │  agent loop: observe → think → act
     │  each tool call hits NarcHook → WonkHook
     │  each AgentEvent → JSON line on stdout
     ▼
   Big Smooth captures stdout, re-emits as ServerEvent on WebSocket
     │
     ▼
   User UI streams: ToolCallStart, TokenDelta, ToolCallComplete, ...
     │
     ▼
   On Completed event:
     - Diver.complete(pearl_id)  OR  pearl_store.close([pearl_id])
     - teammate marked done
     - sandbox optionally torn down (keep_alive flag)
```

## DispatchOptions

The dispatch entry point takes:

| Field            | Meaning                                                                 |
| ---------------- | ----------------------------------------------------------------------- |
| `message`        | The task prompt the agent receives                                       |
| `model`          | Optional override; falls back to provider default                        |
| `budget`         | Cost cap in USD (optional)                                               |
| `working_dir`    | Host path the operator's workspace bind mounts to                        |
| `image`          | OCI image for the operator VM (sandboxed only; rarely overridden)        |
| `keep_alive`     | Hold the sandbox open after `Completed` for debugging (sandboxed only)   |
| `memory_mb`      | Sandbox memory (default 4096)                                            |
| `agent`          | Named agent role (`fixer`, `tester`, etc.); defaults to `fixer`          |
| `pearl_id`       | Caller-supplied pearl id when the chat agent already created one         |
| `prior_messages` | Resume context — prior session messages re-played into the conversation |

## Pearl resolution

Three cascading paths:

1. **Caller supplied.** The chat-agent's `teammate_spawn` tool created the pearl already. Reuse it, flip status to `in_progress`.
2. **Diver available.** Call `Diver.dispatch(title, description, parent)` — creates pearl, marks in_progress, returns ID.
3. **No Diver.** Fall back to `PearlStore::create` directly, then `update(status=in_progress)`.

A single side effect: the `Task: <truncated message>` pearl shows up in `th pearls list`.

## Operator env (sandboxed)

The runner reads its task and config from env vars (no file I/O):

| Var                         | Source                                                   |
| --------------------------- | -------------------------------------------------------- |
| `SMOOTH_TASK_FILE`          | Path inside VM to a bind-mounted tempfile with the task   |
| `SMOOTH_API_URL`            | LLM gateway base URL (from `providers.json`)              |
| `SMOOTH_API_KEY`            | Bearer token                                              |
| `SMOOTH_MODEL`              | Resolved model id                                         |
| `SMOOTH_BUDGET_USD`         | Cost cap                                                  |
| `SMOOTH_WORKSPACE`          | `/workspace` (mount root)                                  |
| `SMOOTH_OPERATOR_ID`        | UUID; used in log lines                                   |
| `SMOOTH_AGENT`              | Role name (`fixer`, `tester`, …)                          |
| `SMOOTH_NARC_URL`           | Routable host IP for the boardroom Narc                   |
| `SMOOTH_ARCHIVIST_URL`      | Host-facing Archivist URL (forwarder destination)         |
| `SMOOTH_PEARL_ID`           | Pearl id (for mailbox + comment tap)                       |
| `SMOOTH_HOST_TOKEN`         | Bearer for `host_tool` calls back to Big Smooth (gated)   |
| `SMOOTH_WORKFLOW`           | `1` enables multi-phase workflow (default)                 |
| `SMOOTH_WORKFLOW_SKIP_TEST` | Bench knob — skip TEST phase to keep scoring stable       |

## Operator env (direct)

A subset of the above. No `SMOOTH_NARC_URL` (Narc is in-process); `SMOOTH_WORKSPACE` defaults to the host cwd unless overridden; no bind mounts.

## AgentEvent → ServerEvent

The runner emits `AgentEvent`s as JSON lines on stdout. Big Smooth parses each line and forwards a matching `ServerEvent` to every WebSocket subscriber:

| AgentEvent          | ServerEvent              | UI effect                  |
| ------------------- | ------------------------ | -------------------------- |
| `IterationStart`    | `IterationStart`         | Heartbeat / progress       |
| `LlmRequest`        | `LlmRequest`             | "Asking model…"            |
| `TokenDelta`        | `TokenDelta`             | Streamed text into chat    |
| `ToolCallStart`     | `ToolCallStart`          | Tool card appears          |
| `ToolCallComplete`  | `ToolCallComplete`       | Tool card resolves         |
| `Completed`         | `TaskCompleted`          | Pearl closes, teammate done |
| `Error`             | `TaskError`              | Banner + pearl close        |

Stderr from the runner is also captured and forwarded as `TokenDelta`s with a `[stderr]` prefix.

## Resume

When a pearl already has prior session messages (from a previous dispatch on the same pearl), `build_resumption_context` reads the last N (default 20) and prepends a `## Resumption context` block to the task message. The agent sees what was done before and continues from there.

## Comment tap

For each dispatched pearl, Big Smooth spawns a `comment_tap` tokio task that watches the pearl's comments and re-emits them as WebSocket events scoped to the teammate. This is how the sidebar shows live operator output even when the dashboard wasn't open at dispatch time.

## Related

- [[Architecture-Overview]]
- [[The-Cast]]
- [[Operators]]
- [[Pearls]]
