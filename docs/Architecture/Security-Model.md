# Security Model

#architecture #security

> [!warn] Trusted-environment posture today
> The microVM boundary is gone. Big Smooth runs the operative as a **host subprocess with host-level access** to your working directory — no VM, no kernel-enforced egress, no filesystem sandbox. The surviving in-process guard is **Narc** tool surveillance plus role-scoped tool gating. Run Smooth where you'd run any coding agent you trust on your machine, on loopback or a trusted tailnet. The auto-mode permission engine (`th-515a13`) and the daemon's kernel-sandbox direction (`th-c89c2a`) are rebuilding real enforcement — both **in progress**, documented below as planned.

## What enforces things today

Every tool call the operative makes passes two in-process hooks on its `ToolRegistry`, in order:

1. **`PermissionHook`** (from the `smooth-operator` engine) — role-scoped tool gating. The operative's agent role (`SMOOTH_AGENT`, default `fixer`) declares which tools and permissions it's allowed; the hook blocks anything outside that surface. This is capability scoping, not a network/FS boundary.
2. **`NarcHook`** (`smooth-narc`) — surveillance. Fast regex detectors run on every call:
   - **Secret detection** — 10 patterns (API keys, tokens, private-key headers, …). A tool call or result carrying a secret is flagged/blocked.
   - **Prompt-injection guard** — 6 patterns over incoming content.
   - **Write guard** — optional (`SMOOTH_NARC_WRITE_GUARD=1`); off by default because the workspace is expected to be written.
   - Ambiguous cases can escalate to an **LLM judge** (`smooth-judge` slot) for a yes/no verdict.

`smooth-policy` TOML is still parsed by the operative, but only to log the intended network allowlist and phase and to feed Narc's surveillance context. **It no longer enforces anything** — the Wonk rule engine and the Goalie iptables/FUSE proxy that turned that policy into a kernel boundary were removed.

## What was removed (July 2026, `th-f4a801`)

- Per-task **microsandbox microVM** — hardware isolation around the agent.
- **Wonk** — the deterministic policy authority (`allow`/`deny` over network, filesystem, tools, pearls, MCP, CLI).
- **Goalie** — the forward proxy that forced all egress through Wonk, enforced by iptables (only Goalie's UID could egress) and a FUSE mount on the workspace.
- The credential-broker plumbing (`smooth-host-stub`, `smooth-credential-helper`, `smooth-bootstrap-bill`) that brokered host creds into the VM.

Git history at the teardown PR's parent commit (`5ccdd51`) is the archive for all of it.

## Planned — auto-mode permission engine (`th-515a13`, in progress)

The near-term replacement for Wonk's interactive posture: a Claude-Code-style **three-way verdict** (`allow` / `deny` / `ask`) on `smooth-policy`, with `ask` surfaced as an interactive prompt in chat and the decision persisted at a chosen scope (`Once`, `Session`, `PearlProject`, `User`). Policy sources stack, narrowest scope wins. This is being built in parallel now; check the pearl for landed status.

## Planned — kernel sandbox direction (`th-c89c2a`)

The [[Daemon-Direction|daemon epic]]'s security verdict: the load-bearing boundary becomes the **kernel**, not the permission layer (a reasoning agent can talk its way past an intent layer, but not past kernel FS/network scoping). The target:

- **Gate 1** — a deterministic rule engine (wonk's successor) in-process via the engine hooks: `deny → ask → allow`, first match wins, `ask` realized by the hook awaiting an approval result (fail-closed on deny/timeout).
- **Gate 2** — an LLM classifier (narc's judge) in `auto` mode only, run out-of-process with an rlimit cap, fail-closed on timeout.
- **Kernel sandbox** — sandbox the **tool subprocesses, never the daemon** (Codex's model): macOS Seatbelt / `sandbox-exec`, Linux `bubblewrap` + Landlock + seccomp; plus an egress-allowlist proxy as the real network boundary.

Threat model: single-tenant (one trusted operator per instance). The real risk isn't a malicious tenant — it's prompt-injection / untrusted repo content turning the operator's own agent against them (the "lethal trifecta": private-data access + untrusted content + egress). The kernel sandbox + egress allowlist + auto-mode is the cheaper, correct defense for that.

## Trusted-integration exceptions to the kernel sandbox

Some OS integrations cannot run inside the tool sandbox at all: macOS EventKit
reaches `calaccessd`/`tccd` over XPC + mach lookups that the seatbelt profile
denies, so a calendar read through the sandboxed `bash` fails 100% of the time.
The `calendar` tool (macOS, pearl th-94cc4a) is therefore the first **documented
exception** — it spawns `ical` with a plain `Command`.

What keeps the exception narrow, and what any future one must also do:

- **argv only, no shell** — no interpolation or injection path.
- **fixed binary** — a resolved install path, never caller-supplied.
- **verb allowlist** — read-only commands; mutation is a separate decision.
- **still a normal tool call** — the permission gate and Narc hook see it exactly
  like every other tool, so surveillance and policy are unchanged. Only the
  *kernel* layer is waived, and only for this one binary.

The TCC grant itself is the compensating control: the OS asks the human once,
per app bundle, and the user can revoke it in System Settings.

## Related

- [[The-Cast]]
- [[Dispatch]]
- [[Daemon-Direction]]
- [[Operatives]]
