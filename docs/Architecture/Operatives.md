# Operatives

#architecture

> [!arch] The agents that actually do the work
> An operative is a `smooth-operative` process running the `smooth-operator` agent engine with a scoped tool surface, hooked into Narc surveillance. One operative per dispatched pearl. It runs on the **host** against your working directory (no microVM, no bind mount) and streams `AgentEvent`s as JSON-lines to Big Smooth.
>
> **Naming:** the *operative* is the worker binary that runs a pearl. The *`smooth-operator` engine* (crate `smooth-operator-core`) is the agent framework it runs. Don't conflate them — and don't confuse either with the public `smooth-operator` service.

## The operative binary

`crates/smooth-operative/` is a standalone Rust binary — the only crate that runs the agent loop in production. It's the native host build (`target/release/` or `target/debug/`), exec'd as a subprocess. There is no cross-compile, musl target, or image step anymore.

Build it:

```bash
cargo build -p smooai-smooth-operative --release
```

Big Smooth resolves it via `$SMOOTH_OPERATIVE_NATIVE`, then `target/{release,debug}/smooth-operative`, then `$CARGO_HOME/bin/`. If it's missing, dispatch closes pearls with `cost_usd=0` and Big Smooth logs a loud warning — see [[Dispatch#Resolving-the-operative-binary]].

## What the operative does on boot

1. Reads its config from env vars in one pass — task (`SMOOTH_TASK` or `SMOOTH_TASK_FILE`), `SMOOTH_API_URL` / `SMOOTH_API_KEY`, `SMOOTH_MODEL`, `SMOOTH_AGENT` (role), budget/iteration caps, `SMOOTH_WORKSPACE`.
2. Builds an `LlmConfig` — OpenAI-compatible by default, switching to native Anthropic request shape for Claude-class models (the only path where multi-turn tool flows survive intact).
3. Constructs the `ToolRegistry` scoped to the workspace and the agent role.
4. Installs `PermissionHook` (role-scoped tool gating) and `NarcHook` (secret / injection / optional write-guard surveillance). See [[Security-Model]].
5. Runs the coding workflow (for coding roles with bash permission) or the single-agent loop, emitting one JSON `AgentEvent` per line on stdout. Diagnostics go to stderr only — any non-JSON stdout line is dropped by Big Smooth.
6. Exits 0 on `Completed`, non-zero on error (last line `{"type":"Error","message":"…"}`).

## The agent loop (smooth-operator)

The engine provides the framework:

| Module            | Job                                                              |
| ----------------- | ---------------------------------------------------------------- |
| `agent.rs`        | Observe → think → act loop; emits `AgentEvent`s through a channel |
| `llm.rs`          | Chat completions (OpenAI-compatible + native Anthropic), streaming |
| `tool.rs`         | `Tool` trait + `ToolRegistry` with pre/post `ToolHook`s          |
| `conversation.rs` | Message history, token estimation, context-window trimming       |
| `checkpoint.rs`   | Groove checkpoint store; configurable strategies                 |

## The coding workflow

For a coding role (`Activity::Coding` with `bash` permission), the operative runs `smooth_cast::coding_workflow::run_coding_workflow` — a single-agent loop with a thin outer governor that feeds the previous turn's test output back in, snapshots the workspace when failing-test counts drop, and stops on the first convincing signal (green / close-to-green plateau / budget / iteration ceiling). Other roles, or coding opt-out (`SMOOTH_WORKFLOW=0`), fall back to a single `Agent::run_with_channel` pass. (The earlier seven-phase ASSESS/PLAN/EXECUTE/… pipeline was dropped — it kept short-circuiting at one detector or another.)

## The built-in tool surface

The operative registers file tools (`read_file`, `write_file`, `edit_file`, `apply_patch`, `list_files`, `grep`), `bash` plus background-process tools (`bg_run` / `bg_status` / `bg_logs` / `bg_kill`), `lsp` and `project_inspect`, memory tools (`read_memory` / `write_memory`), a `todo_list`, `skill_use` (invoke a named skill from the system-prompt catalog — `th-e0f812`), `http_fetch` and (optionally) `web_search`, `forward_port`, `reply_to_chat` and `ask_smooth` (escalate to Big Smooth), `host_tool` (proxy a whitelisted host CLI via `SMOOTH_HOST_TOKEN`), a `delegate` tool that spawns a sub-pearl / child operative, and the pearl read/write tools. Plus any [MCP servers](../extending.md) and [CLI-wrapper plugins](../extending.md) configured via `mcp.toml` / `plugin.toml`.

## Mailbox + steering

While an operative is live, the user can push messages to it over the WebSocket — `th steer <pearl_id> "message"` posts a `steer` comment; `th pause` / `th resume` / `th cancel` similarly. The operative's mailbox poller reads new comments at the start of each iteration and surfaces them to the agent as a tool result; the agent decides what to do.

## Lifecycle

- **Spawn** — Big Smooth's dispatch path exec's the operative as a host subprocess.
- **Run** — the operative streams events; Big Smooth re-emits them as WebSocket `ServerEvent`s; the teammate registry tracks status.
- **Complete** — the operative emits `Completed`; Big Smooth marks the pearl done via Diver and closes the comment tap.
- **Error** — the operative emits `Error`; Big Smooth closes the pearl and sends `TaskError` to subscribers.
- **Cancel** — the user sends cancel; Big Smooth kills the subprocess.

## Related

- [[Dispatch]]
- [[The-Cast]]
- [[Security-Model]]
- [[Pearls]]
