# What Is Smooth

#start-here

> [!arch] One sentence
> Smooth is a Rust binary (`th`) that runs an AI agent stack — orchestrator, surveillance cast, operatives — as a daemon on your machine.

## What `th up` actually does

`th up` (no subcommand) does this:

1. Daemonizes and brings up [[Architecture/The-Cast#Big-Smooth|Big Smooth]] plus the rest of the [[Architecture/The-Cast|cast]] as tokio tasks in one host process.
2. Serves the API, WebSocket, and embedded web UI on `:4400`.
3. Writes `~/.smooth/smooth.pid` so `th down` can find it later.

That's the whole user experience. There is no VM to boot, no OCI image to pull, no Docker container, no `th vm` subsystem. (The microVM sandboxed mode was removed 2026-07 — see [[Decisions/ADR-004-remove-microvm-sandbox-stack]].)

See [[Operations/Running-Locally]] for the knobs.

## What gets dispatched

Once Smooth is up, you talk to it via the embedded web UI at `http://localhost:4400`, the `th code` TUI, or the WebSocket API. You ask for work. Big Smooth turns the request into one or more pearls (work items) and dispatches **operators** to do them.

An operative is the [`smooth-operative`](../../crates/smooth-operative/) binary exec'd as a host subprocess, running an agent loop with a scoped tool surface. The operative's tools (read, write, bash, etc.) are wrapped in hooks that call out to [[Architecture/The-Cast#Narc|Narc]] for surveillance and [[Architecture/The-Cast#Scribe|Scribe]] for structured logging — both in-process.

See [[Architecture/Dispatch]] for the dispatch flow, [[Architecture/Operatives]] for the operative.

## What it's for

- **Coding agents on your machine, watched.** Regex + LLM judge on the tool surface: CliGuard blocks dangerous shell patterns, detectors screen for secrets and prompt injection. Note there is no VM boundary anymore — tools execute against the host, so run it in environments you trust.
- **Dispatchable AI teammates.** Pearls + Diver give you a work item tracker the agent can read and write. The chat agent spawns teammates by creating pearls.
- **A benchmarkable substrate.** `th bench` runs Exercism-style problems through the agent loop with deterministic scoring. See [[Engineering/Bench-Harness]].

## What it isn't

- Not a hosted service. Everything runs on your machine.
- Not Docker. No container runtime is required or invoked.
- Not a sandbox. Hardware isolation was removed with the microVM stack ([[Decisions/ADR-004-remove-microvm-sandbox-stack]]); isolation is expected to return in a different shape via the smooth-daemon work (th-c89c2a, th-515a13).
- Not multi-tenant. One user, one daemon per host.

## Related

- [[Home]]
- [[Start-Here/Glossary]]
- [[Architecture/Architecture-Overview]]
- [[Operations/Running-Locally]]
