# Operatives

#architecture

> [!arch] The agents that actually do the work
> An operative is a `smooth-operative` process running the `smooth-operator` agent engine with a scoped tool surface, hooked into the cast. One operative per dispatched pearl. Streams `AgentEvent`s as JSON-lines to its parent.
>
> **Naming:** the *operative* is the sandboxed worker (the microVM-resident binary that runs a pearl). The *`smooth-operator` engine* (crate `smooai-smooth-operator`, being extracted to `smooth-operator-core`) is the agent framework the operative runs. Don't conflate them — and don't confuse either with the public `smooth-operator` service.

## The operative binary

`crates/smooth-operative/` is a standalone Rust binary. It is the only crate that runs the agent loop in production.

- **Sandboxed mode:** cross-compiled to `aarch64-unknown-linux-musl`, baked into the Safehouse image at `/opt/smooth/bin/`, and exec'd inside the microVM.
- **Direct mode:** the native build (host triple) from `target/release/` or `target/debug/`.

Build it with:

```bash
scripts/build-operative.sh    # cross-compile to musl (sandboxed)
cargo build -p smooai-smooth-operative --release    # native (direct)
```

A one-time dev setup is required for the cross-compile:

```bash
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild
pip3 install ziglang
```

## What the operative does on boot

1. Reads its config from env vars in a single pass. No file I/O for config.
2. Loads the task message from `SMOOTH_TASK_FILE` (bind-mounted) or `SMOOTH_TASK` (env).
3. Builds an `LlmConfig` from `SMOOTH_API_URL`, `SMOOTH_API_KEY`, `SMOOTH_MODEL`.
4. Constructs the `ToolRegistry` scoped to `SMOOTH_WORKSPACE`.
5. Installs the `NarcHook` (pre/post tool-call surveillance).
6. Installs the `WonkHook` (policy check for every tool call before it runs).
7. Wires the in-VM Goalie as the `HTTP_PROXY` so LLM and HTTP-tool calls flow through Wonk-mediated egress.
8. Runs `Agent::run_with_channel`, emitting one JSON-encoded `AgentEvent` per line on stdout.
9. Exits 0 on `Completed`, non-zero on error. The last line on error is `{"type":"Error","message":"…"}`.

## The agent loop (smooth-operator)

`smooth-operator` provides the framework:

| Module          | Job                                                              |
| --------------- | ---------------------------------------------------------------- |
| `agent.rs`      | Observe → think → act loop; emits `AgentEvent`s through a channel |
| `llm.rs`        | OpenAI-compatible chat completions, streaming                    |
| `tool.rs`       | `Tool` trait + `ToolRegistry` with pre/post `ToolHook`s          |
| `conversation.rs` | Message history, token estimation, context-window trimming     |
| `checkpoint.rs` | The Groove checkpoint store; configurable strategies             |

## The built-in tool surface

The operative registers:

- `read_file(path, offset?, limit?)` — read under workspace, line ranges allowed
- `write_file(path, content)` — write under workspace; NarcHook secret + write-guard filters
- `apply_patch(path, patch)` — fuzzy-match patch application
- `list_files(path?, recursive?)` — directory listing
- `search_files(query, path?, file_pattern?)` — ripgrep-style search
- `bash(command, timeout_secs?)` — shell exec; output bounded
- `ask_smooth` — escalate a clarifying question to Big Smooth (sandbox-only IPC)
- `host_tool(name, args)` — proxy a whitelisted host CLI (gh, git, kubectl, …) via `SMOOTH_HOST_TOKEN`
- `delegate(pearl_title, message)` — spawn a sub-pearl, kicks off a child operative
- `reply_to_chat(message)` — write a message back to the user's chat
- `pearls_*` — read and write pearls in the project Dolt store
- `mailbox_*` — read steering / chat messages addressed to this operative

Plus any [MCP servers](../../docs/extending.md) configured via `mcp.toml` and any [plugins](../../docs/extending.md) declared via `plugin.toml`.

## Workflow (multi-phase)

When `SMOOTH_WORKFLOW=1` (the default), the operative runs a multi-phase loop:

```
   plan → execute → test → review
```

Each phase is a separate `Agent::run` call with a different system prompt and tool subset. `SMOOTH_WORKFLOW_SKIP_TEST=1` skips the TEST phase (used by the bench harness to keep scoring stable). `SMOOTH_WORKFLOW=0` falls back to the single-Agent loop.

## Mailbox + steering

While an operative is live, the user can push messages to it via the WebSocket:

- `th steer <pearl_id> "message"` → posts a comment of type `steer` on the pearl
- `th pause <pearl_id>` / `th resume <pearl_id>` / `th cancel <pearl_id>` similarly

The operative's mailbox poller reads new comments at the start of each iteration and surfaces them to the agent through a tool result. The agent decides what to do.

## Lifecycle

- **Spawn:** Big Smooth's dispatch path exec's the operative (subprocess in direct mode, microsandbox exec in sandboxed mode).
- **Run:** operative streams events; Big Smooth re-emits as WebSocket `ServerEvent`s; teammate registry tracks status.
- **Complete:** operative emits `Completed`; Big Smooth marks pearl done via Diver (or directly), closes the comment tap.
- **Error:** operative emits `Error`; Big Smooth closes the pearl and sends `TaskError` to subscribers.
- **Cancel:** user sends cancel; Big Smooth tears down the operative (subprocess kill in direct mode, sandbox destroy in sandboxed mode).

## Related

- [[Dispatch]]
- [[The-Cast]]
- [[Pearls]]
- [[../crates/smooth-operative]]
