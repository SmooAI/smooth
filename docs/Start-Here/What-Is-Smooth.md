# What Is Smooth

#start-here

> [!arch] One sentence
> Smooth is a Rust binary (`th`) that runs an AI agent stack on your machine — an always-on orchestrator (Big Smooth) that dispatches operatives to do real work, with Narc tool surveillance on the agent's tool surface.

## What `th up` actually does

`th up` (the default, no subcommand):

1. Starts **Big Smooth** — an axum HTTP + WebSocket server — as a host process and daemonises it (`--foreground` to stay attached).
2. Binds `127.0.0.1:4400` by default. `--bind 0.0.0.0` exposes it, but the API is **unauthenticated today** (pearl `th-6db839`), so keep it on loopback or a trusted tailnet.
3. Opens the pearl store and brings up the in-process cast (Diver, Archivist).

`th down` stops it (kills the pid). There's no VM, no Docker container, no image pull. Big Smooth is just a process on your host. (Until July 2026 `th up` booted a microsandbox microVM per the old architecture; that was removed in pearl `th-f4a801` — see [[Decisions/ADR-004-remove-microvm-sandbox-stack]].) See [[Operations/Running-Locally]] for the knobs.

## What gets dispatched

You talk to Smooth via the embedded web UI at `http://localhost:4400`, the `th code` TUI, or the WebSocket API. You ask for work; Big Smooth turns the request into one or more pearls (work items) and dispatches **operatives** to do them.

An operative is the [`smooth-operative`](../../crates/smooth-operative/) binary running an agent loop with a scoped tool surface, spawned as a host subprocess against your working directory. Its tools (read, write, bash, …) pass through two in-process hooks: role-scoped tool gating and [[Architecture/The-Cast#Narc|Narc]] surveillance (secret + prompt-injection detection). See [[Architecture/Dispatch]] and [[Architecture/Operatives]].

## What it's for

- **A coding agent you run on your own machine.** Big Smooth dispatches an operative that compiles, tests, installs dev deps, and iterates against your repo, streaming every token, tool call, and cost back to the UI.
- **Dispatchable AI teammates.** Pearls + Diver give you a work-item tracker the agent reads and writes; the chat agent spawns teammates by creating pearls.
- **A benchmarkable substrate.** `th bench` runs Exercism-style problems through the agent loop with deterministic scoring. See [[Engineering/Bench-Harness]].

## Security posture

The operative runs with **host-level access** — there is no VM boundary today. Narc surveillance and role-scoped tool gating are the in-process guards; real enforcement (an auto-mode permission engine, `th-515a13`, and a kernel tool-subprocess sandbox, `th-c89c2a`) is in progress. Run Smooth where you'd run any trusted coding agent. See [[Architecture/Security-Model]].

## What it isn't

- Not a hosted service. Everything runs on your machine.
- Not Docker. No container runtime required.
- Not a sandbox. Hardware isolation was removed with the microVM stack ([[Decisions/ADR-004-remove-microvm-sandbox-stack]]); isolation is expected to return in a different shape via the smooth-daemon work (`th-c89c2a`, `th-515a13`).
- Not multi-tenant. One trusted operator per instance — that single-tenant model is why the microVM was dropped (see [[Architecture/Daemon-Direction]]).

## Related

- [[Home]]
- [[Start-Here/Glossary]]
- [[Architecture/Architecture-Overview]]
- [[Operations/Running-Locally]]
