# Extension System (SEP)

#architecture #planned

> [!info] Planned — phases landing now
> **SEP** (Smooth Extension Protocol) is the planned extension system that gives `smooth-operator` pi-style extensibility: tools, slash commands, event middleware, UI widgets, and providers, added without rebuilding the binary. It is being built incrementally (epic `th-2def2a`). **Phase 0** (wire protocol, spec/fixture harness, host lifecycle — `th-6d1794`) is merged in the `smooth-operator` engine repo; **Phase 1** (`registerTool` end-to-end + the TypeScript SDK v0) is landing now. Everything below is design + phasing — in-progress, not fully shipped. The zero-code extension tiers that exist **today** (MCP servers, CLI-wrapper plugins) are covered in [`docs/extending.md`](../extending.md).

## Why a protocol, not in-process plugins

pi loads TypeScript extension factories in-process. Rust can't import a `.ts`, and in-process trait plugins are exactly what SEP rejects (the existing `smooth-plugin` trait crate is slated for deletion — zero consumers). Three decisions are locked:

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

`@smooai/smooth-extension-sdk` (TypeScript, in the smooth-operator repo) mirrors pi's `ExtensionAPI` by name so pi extensions port near-mechanically. Zod v4 schemas (wire truth is JSON Schema), TypeBox pass-through. Testing via `createTestHost` (in-process scripted host) + `runConformance` (real subprocess against the shared fixtures). Scaffold: `create-smooth-extension`.

## Relationship to what exists today

| Existing surface | SEP verdict |
| --- | --- |
| `plugin.toml` CLI wrappers | **Keep** — the zero-code declarative tier |
| `mcp.toml` / rmcp | **Sibling standard** — keep the bridge |
| `smooth-cast` skills | **Unify** — extension `[resources] skills` feeds smooth-cast discovery; smooth-cast stays canonical |
| `smooth-plugin` trait crate | **Delete** — zero consumers; in-process traits are what SEP rejects |

## Related

- [[Daemon-Direction]]
- [[Architecture-Overview]]
- [`docs/extending.md`](../extending.md) — MCP + plugin tiers available today
