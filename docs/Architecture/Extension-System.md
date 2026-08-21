# Extension System (SEP)

#architecture #planned

> [!info] Planned — phases landing now
> **SEP** (Smooth Extension Protocol) is the planned extension system that gives `smooth-operator` pi-style extensibility: tools, slash commands, event middleware, UI widgets, and providers, added without rebuilding the binary. It is being built incrementally (epic `th-2def2a`). **Phase 0** (wire protocol, spec/fixture harness, host lifecycle — `th-6d1794`) is merged in the `smooth-operator` engine repo; **Phase 1** (`registerTool` end-to-end + the TypeScript SDK v0) is landing now. Everything below is design + phasing — in-progress, not fully shipped. The zero-code extension tiers that exist **today** (MCP servers, CLI-wrapper plugins) are covered in [`docs/extending.md`](../extending.md).

## Why a protocol, not in-process plugins

pi loads TypeScript extension factories in-process. Rust can't import a `.ts`, and in-process trait plugins are exactly what SEP rejects (the old `smooth-plugin` trait crate — zero consumers — was deleted in Phase 5). Three decisions are locked:

1. **Runtime** — extensions are long-lived **subprocesses speaking JSON-RPC 2.0 over stdio** (ndjson, one message per line). Any language; a TypeScript SDK is the DX centerpiece. Same framing as MCP stdio (the `smooth-operative` rmcp bridge is the precedent), debuggable with `jq`.
2. **Scope** — **full pi parity** as the end state, phased across many PRs.
3. **Host** — the host lives at **engine level** in `smooth-operator-core`, so the five polyglot engine builds, the Big Smooth daemon, and the operative all inherit it. The protocol is the spec; a shared conformance-fixture suite keeps every host honest.

## Shape of the protocol

- **Lifecycle**: spawn → `initialize` (capabilities, workspace + trust, session, mode/UI caps) → registrations (tools/commands/flags/subscriptions) → steady state → `shutdown` (5s grace, then SIGKILL).
- **Host → ext**: `event` (observe, fire-and-forget), `hook` (intercept, awaited), `tool/execute` (streaming `tool/update` + `$/cancel`), `command/execute`, `provider/*`, `ping`.
- **Ext → host**: `tools/register|setActive` (clamped to the per-agent enabled tools — never widens auth), `session/*`, `exec/run` (audited through the host permission engine), `ui/*`, `kv/*`, `events/publish` (inter-extension bus), `log`.
- **Hooks** mirror pi and chain in load order, fail-closed on the security-relevant ones (`tool_call`, `user_bash`) and fail-open on the rest. Two context tiers (`event` / `command`) guard against session-mutation deadlocks.
- **Versioning**: an independent integer `protocolVersion`, decoupled from engine semver; handshake negotiates `min(host, ext)`; unknown fields ignored; per-extension load failure tolerated.

## Manifest + trust

`extension.toml` in `~/.smooth/extensions/<name>/` (global) and `<repo>/.smooth/extensions/<name>/` (project wins) — the same merge rule as `mcp.toml` / `plugin.toml`. Declares command/args/env, capabilities, resources (skills/prompts/themes), and hook timeouts. Trust is host-owned (extensions can't vote on their own trust): project extensions load only in trusted workspaces; first-run prompt shows declared capabilities, recorded by source + content-hash. Headless/daemon pre-trusts via `th ext trust` or config, else silently doesn't load (fail-safe for unattended runs).

## SDK

`@smooai/smooth-extension-sdk` (TypeScript, in the smooth-operator repo) mirrors pi's `ExtensionAPI` by name so pi extensions port near-mechanically. Zod v4 schemas (wire truth is JSON Schema), TypeBox pass-through. Testing via `createTestHost` (in-process scripted host) + `runConformance` (real subprocess against the shared fixtures). Scaffold a new extension with `npm create smooth-extension` (Phase 5) — templates: `tool`, `permission-gate`, `command`, `provider-less`; scaffolded output passes `runConformance` out of the box.

## Packaging & discovery (Phase 5)

`th ext install` takes a local dir, `npm:@scope/pkg[@version]`, or `git:host/user/repo[@ref]`. Packaged sources vendor under `~/.smooth/extensions/.npm` / `.git/<host>/<path>` and are surfaced to the engine's (top-level-only) discovery via a `~/.smooth/extensions/<name>` symlink, so packaged and local installs load identically. A manifest may be `extension.toml` or a `package.json` `smooth` key (synthesized at install). `th ext update` re-fetches packaged extensions from their recorded source, re-locking trust on a changed manifest. `th ext search` matches a curated in-binary index plus live npm packages tagged `smooth-extension`.

## UI relay across frontends (Phase 6)

An extension's `ui/*` request is capability-negotiated at handshake (`mode` +
capability list). The extension owns the content, the host owns routing, and
each frontend owns rendering:

- **smooth-code TUI** — renders the full set inline (Phase 3).
- **smooth-web via the daemon** — a dispatched operative runs headless, so its
  `HttpUiProvider` relays each `ui/*` request to Big Smooth over the existing
  `SMOOTH_NARC_URL` + `SMOOTH_HOST_TOKEN` callback channel (`POST /api/ui/request`,
  same channel `host_tool` uses). The daemon broadcasts a `UiRequest`
  [`ServerEvent`] to every connected client and, for the interactive kinds
  (`select`/`confirm`/`input`), blocks the operative's call until a browser
  answers via `POST /api/ui/answer`. The smooth-web `UiRelay` component renders
  `select`/`confirm`/`input` as a modal, `notify` as a toast, and
  `set_status`/`set_widget`/`set_title` in the chrome; render blocks
  (`markdown`/`keyvalue`/`progress`/`table`/`diff`/`stack`, each with a `text`
  fallback) render natively. Unattended (no client connected) the request
  resolves to `{cancelled:true}` rather than hang; under `SMOOTH_AUTO_MODE=bypass`
  a `confirm` is auto-answered `{confirmed:true}` (audited); otherwise it waits
  up to `SMOOTH_UI_TIMEOUT_SECS` (default 120s), then cancels.
  Source: `smooth-bigsmooth/src/ui_relay.rs`, `smooth-operative/src/ui_relay_provider.rs`,
  `smooth-web/web/src/components/UiRelay.tsx`.
- **chat-widget on the public servers** — `select`/`confirm` render as
  chat-native button frames (smooth-operator repo).

## Big Smooth's own chat loop (pearl th-6d8606)

The assistant itself hosts extensions — not just the operatives it dispatches.
At daemon startup, `sep::init_chat_extension_host` discovers global + cwd
extensions, loads the **pre-trusted** ones (never a trust prompt — the daemon
is unattended; run `th ext trust` first) into ONE daemon-lifetime
`ExtensionHost`, and every chat turn attaches it (`Agent::with_extension_host`
against the registry `build_chat_tools` returns). Consequences:

- **Tools** — extension tools (`<ext>.<tool>`) sit alongside the pearl/teammate
  tools and pass through the same AutoMode permission hook and Narc
  surveillance hook, in that order.
- **UI** — the chat loop runs IN the daemon, so its delegate
  (`sep::DaemonUiProvider`) calls the relay in-process (no HTTP-to-self),
  broadcasting with `task_id = "big-smooth-chat"`; timeout / bypass
  auto-confirm / audit behavior is byte-identical to the operative relay.
- **Slash commands** — a chat message shaped `/cmd args` (or `/ext:cmd args`)
  that matches a registered extension command executes command-tier and
  returns the extension's text as the assistant reply, no LLM turn. Unmatched
  slash text falls through to the agent. Arguments arrive as
  `{ "args": "<raw remainder>" }`.
- **Reload** — `th ext reload <name>` now POSTs `/api/ext/reload` on the local
  daemon (best-effort) so the live host respawns the extension immediately;
  the next chat turn picks up fresh tool proxies. `GET /api/ext` lists loaded
  extensions + commands. Newly _installed_ extensions still need a daemon
  restart (discovery runs at startup).
- **Lifetime** — one shared host across chat sessions (a per-turn host would
  respawn every subprocess per message). Extension in-process state therefore
  spans sessions; per-session hosts are the upgrade when the daemon epic's
  durable sessions land (th-c89c2a).

## Relationship to what exists today

| Existing surface                     | SEP verdict                                                                                                        | Status         |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------ | -------------- |
| `plugin.toml` CLI wrappers           | **Keep** — the zero-code declarative tier                                                                          | —              |
| `mcp.toml` / rmcp                    | **Sibling standard** — keep the bridge                                                                             | —              |
| `smooth-cast` skills                 | **Unify** — extension `[resources] skills` feeds smooth-cast discovery (trusted only); smooth-cast stays canonical | Done (Phase 5) |
| `smooth-code` duplicate skill parser | **Delete** — migrate to smooth-cast; `/skill:name` stays as frontend sugar                                         | Done (Phase 5) |
| `smooth-plugin` trait crate          | **Delete** — zero consumers; in-process traits are what SEP rejects                                                | Done (Phase 5) |

## Related

- [[Daemon-Direction]]
- [[Architecture-Overview]]
- [`docs/extending.md`](../extending.md) — MCP + plugin tiers available today
