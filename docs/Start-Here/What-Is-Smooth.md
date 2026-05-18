# What Is Smooth

#start-here

> [!arch] One sentence
> Smooth is a Rust binary (`th`) that boots an AI agent stack — orchestrator, security cast, operator runners — either inside a microsandbox microVM (default) or directly on the host (escape hatch).

## What `th up` actually does

`th up` (the default, no subcommand) does this:

1. Pulls the Boardroom OCI image (`ghcr.io/smooai/boardroom:latest`).
2. Boots a single microsandbox microVM from that image.
3. Forwards guest `:4400` out to the host so your browser / `th code` can reach the API.
4. Inside the VM, the Boardroom binary brings up [[Architecture/The-Cast#Big-Smooth|Big Smooth]] plus the rest of the [[Architecture/The-Cast|cast]] as tokio tasks.
5. Exits. The VM runs out-of-process; `th down` later tears it back down.

That's the whole user experience. There is no daemon to manage on the host, no Docker container, no persistent named volume, no `th vm` subsystem. The microVM IS Smooth.

`th up direct` does the same thing minus the microVM: everything runs as tokio tasks in a host process and `th up` daemonises itself. Reach for it only inside an already-trusted environment.

See [[Architecture/Sandboxed-Mode]] and [[Architecture/Direct-Mode]] for the full picture.

## What gets dispatched

Once Smooth is up, you talk to it via the embedded web UI at `http://localhost:4400`, the `th code` TUI, or the WebSocket API. You ask for work. Big Smooth turns the request into one or more pearls (work items) and dispatches **operators** to do them.

An operator is the [`smooth-operator-runner`](../../crates/smooth-operator-runner/) binary running an agent loop with a scoped tool surface. The operator's tools (read, write, bash, etc.) are wrapped in hooks that call out to [[Architecture/The-Cast#Wonk|Wonk]] for policy decisions, [[Architecture/The-Cast#Narc|Narc]] for surveillance, and [[Architecture/The-Cast#Scribe|Scribe]] for structured logging — all of which live in the same VM (sandboxed mode) or process (direct mode).

See [[Architecture/Dispatch]] for the dispatch flow, [[Architecture/Operators]] for the runner.

## What it's for

- **Coding agents you actually trust to run on your machine.** Hardware-isolated microVM, kernel-enforced egress proxy, regex + LLM judge on the tool surface. The agent can compile, test, install dev deps, and iterate without permission prompts because the boundary is the VM, not the host.
- **Dispatchable AI teammates.** Pearls + Diver give you a work item tracker the agent can read and write. The chat agent spawns teammates by creating pearls.
- **A benchmarkable substrate.** `th bench` runs Exercism-style problems through the agent loop with deterministic scoring. See [[Engineering/Bench-Harness]].

## What it isn't

- Not a hosted service. Everything runs on your machine.
- Not Docker. No container runtime is required; outbound to a host Docker / OrbStack / Kalima is supported via `allow_host_loopback`, but Smooth never invokes Docker itself.
- Not multi-tenant. One user, one boardroom VM per host.

## Related

- [[Home]]
- [[Start-Here/Glossary]]
- [[Architecture/Architecture-Overview]]
- [[Operations/Running-Locally]]
