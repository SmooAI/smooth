# Smooth: Hardware-Isolated AI Agent Orchestration

## A Security-First Architecture for Autonomous Code Agents

**Version 1.0 — April 2026**
**Smoo AI, Inc.**

---

## Abstract

Smooth is an AI agent orchestration platform that executes autonomous coding agents inside hardware-isolated microVMs. Unlike existing agent frameworks that run with full host access, Smooth enforces a multi-layer security architecture where every tool call, network request, and file write passes through independent security services before execution. This paper describes the architecture, threat model, and enforcement mechanisms.

---

## 1. The Problem: Untrusted AI Agents on Trusted Systems

Modern AI coding agents (Claude Code, Cursor, Aider, etc.) execute with the full permissions of the user who invoked them. They can read any file, execute arbitrary shell commands, make network requests, and modify source code—all without hardware-level isolation.

This creates a fundamental trust gap: the LLM generating tool calls is an external service whose outputs cannot be fully predicted or verified before execution. Prompt injection, hallucinated commands, and emergent misbehavior are not theoretical—they are observed in production.

The industry response has been software-level guardrails: regex pattern matching, user confirmation dialogs, and permission prompts. These are useful but insufficient because they run in the same process and address space as the agent itself. A sophisticated attack or bug can bypass them.

---

## 2. Smooth's Security Architecture

Smooth takes a fundamentally different approach: **hardware isolation first, software enforcement second**.

### 2.1 The Boardroom Model

Every Smooth deployment runs in a layered VM topology:

```
┌─────────────────────────────────────────────────────┐
│                    Host Machine                       │
│                                                       │
│  ┌─────────────────────────────────────────────────┐ │
│  │           The Boardroom (microVM)                │ │
│  │                                                   │ │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐      │ │
│  │  │Big Smooth│  │Archivist │  │  Diver   │      │ │
│  │  │(Boss)    │  │(Logs)    │  │(Pearls)  │      │ │
│  │  └──────────┘  └──────────┘  └──────────┘      │ │
│  └───────────────────────┬─────────────────────────┘ │
│                          │ spawns                      │
│  ┌───────────────────────┼─────────────────────────┐ │
│  │     Operator VM 1     │     Operator VM 2        │ │
│  │  ┌────────────────┐   │  ┌────────────────┐     │ │
│  │  │ Agent Runner   │   │  │ Agent Runner   │     │ │
│  │  │ (LLM + Tools)  │   │  │ (LLM + Tools)  │     │ │
│  │  ├────────────────┤   │  ├────────────────┤     │ │
│  │  │ Wonk (Policy)  │   │  │ Wonk (Policy)  │     │ │
│  │  │ Goalie (Net)   │   │  │ Goalie (Net)   │     │ │
│  │  │ Narc (Audit)   │   │  │ Narc (Audit)   │     │ │
│  │  │ Scribe (Log)   │   │  │ Scribe (Log)   │     │ │
│  │  └────────────────┘   │  └────────────────┘     │ │
│  └───────────────────────┴─────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

**Big Smooth** is a read-only orchestrator. It dispatches tasks, monitors progress, and reviews results. It never executes LLM tool calls directly. It never writes to the user's filesystem. It never makes API calls on behalf of agents.

**Operator VMs** are where agents actually run. Each operator gets its own hardware-isolated microVM with:
- Its own kernel
- Its own filesystem (bind-mounted workspace)
- Its own network stack (filtered by Goalie)
- Its own security cast (Wonk, Goalie, Narc, Scribe)

### 2.2 The Security Cast

Every operator VM runs four independent security services:

| Service | Role | Enforcement |
|---------|------|-------------|
| **Wonk** | Access control authority | TOML policy rules, domain allowlists, filesystem deny patterns, port restrictions |
| **Goalie** | Network proxy | HTTP/HTTPS forward proxy, all traffic routed through Wonk checks |
| **Narc** | Tool surveillance | Secret detection (10 regex patterns), prompt injection guard (6 patterns), write guard |
| **Scribe** | Audit logging | Every tool call, network request, and file write logged with trace context |

These services are **independent processes** communicating over HTTP. A compromised agent cannot disable Wonk because Wonk runs in a separate process with its own address space. A compromised Wonk cannot suppress audit logs because Scribe forwards directly to the Archivist in the Boardroom.

### 2.3 Policy Enforcement

Every operator receives a TOML policy generated for its specific task:

```toml
[metadata]
operator_id = "op-abc123"
phase = "execute"

[network]
allow = [
    { domain = "openrouter.ai" },
    { domain = "registry.npmjs.org" },
]

[filesystem]
writable = true
deny_patterns = ["*.env", "*.pem", "*.key", ".ssh/*", ".aws/*"]

[ports]
enabled = true
allow_range = [1024, 65535]
deny = [22]

[tools]
allow = ["read_file", "write_file", "bash", "forward_port", "delegate"]
deny = ["workflow"]
```

Policies vary by orchestration phase:
- **Assess**: Read-only filesystem, minimal network, no write tools
- **Execute**: Full tool access, writable filesystem, package registries allowed
- **Review**: Read-only, adversarial—only LLM API access, no auto-approve

### 2.4 Enterprise Policy Override

Organizations can deploy a permanent TOML policy at `.smooth/enterprise-policy.toml` that cannot be overridden by any task policy:

```toml
[network]
deny_domains = ["competitor.com", "*.internal.corp"]

[filesystem]
deny_patterns = ["*.pfx", "*.p12", "internal-*"]

[ports]
deny_ports = [22, 3306, 5432]
disabled = false
```

Enterprise deny rules are merged into every task policy. Even if a task policy allows a domain, the enterprise deny takes precedence.

### 2.5 Path Translation

A critical subtlety: operators see guest paths (`/workspace/.env`) but policies protect host paths. Wonk maintains a mount table that translates guest paths to host paths before checking deny patterns. This prevents path-based bypasses where an agent might reference a sensitive file by its guest path to evade host-path deny rules.

---

## 3. Threat Model

### 3.1 Threats Addressed

| Threat | Mitigation |
|--------|-----------|
| Prompt injection causing data exfiltration | Goalie network proxy + Wonk domain allowlist |
| Agent reads secrets (.env, SSH keys, API tokens) | Wonk filesystem deny patterns with path translation |
| Agent executes destructive commands (rm -rf, git push --force) | Narc CLI guard + Wonk writable policy |
| Agent installs malicious packages | Goalie restricts to approved registries |
| Agent communicates with C2 server | All network traffic through Goalie; only allowlisted domains |
| Lateral movement to other services | Hardware VM isolation; no host network access by default |
| Agent modifies its own security config | Wonk/Goalie/Narc run as independent processes in the VM |
| Audit log tampering | Scribe forwards to Archivist in the Boardroom (separate VM) |

### 3.2 Threats Not Addressed (Current Limitations)

- **Side-channel attacks**: Timing attacks against the hypervisor are theoretically possible but require sophistication beyond current LLM capabilities.
- **Hypervisor escapes**: We rely on Apple Hypervisor Framework (macOS) and KVM (Linux) for isolation. Kernel-level exploits are out of scope.
- **LLM provider compromise**: If the LLM API itself is compromised, the agent's behavior is unpredictable. Goalie limits the blast radius.

---

## 4. Implementation

Smooth is implemented as a single Rust binary (`th`) with 17 workspace crates:

- **smooth-bigsmooth**: Orchestrator, API server, sandbox lifecycle
- **smooth-operator**: AI agent framework (LLM client, tools, checkpoints)
- **smooth-policy**: TOML policy types, glob matching, enterprise merge
- **smooth-wonk**: In-VM access control server
- **smooth-goalie**: In-VM HTTP/HTTPS forward proxy
- **smooth-narc**: In-VM tool surveillance (regex + LLM judge)
- **smooth-scribe**: In-VM structured logging
- **smooth-archivist**: Central log aggregator
- **smooth-pearls**: Dolt-backed work item tracker
- **smooth-bootstrap-bill**: microVM lifecycle manager (via Microsandbox)

The entire stack compiles to a single binary with zero runtime dependencies. Operators are cross-compiled to `aarch64-unknown-linux-musl` and mounted into microVMs at runtime.

### 4.1 Key Design Decisions

1. **Rust, not Python**: Memory safety, zero-cost abstractions, and single-binary deployment. No GC pauses during real-time agent monitoring.

2. **microVMs, not containers**: Containers share the host kernel. microVMs (via Microsandbox/libkrun) provide full kernel isolation with sub-second boot times.

3. **TOML policies, not code**: Policies are declarative data, not executable code. They can be audited, diffed, version-controlled, and reviewed by non-engineers.

4. **Independent security services**: Wonk, Goalie, Narc, and Scribe are separate processes, not library hooks. Compromise of one does not compromise the others.

5. **Read-only orchestrator**: Big Smooth never executes agent tool calls. It only reads events and dispatches tasks. This is enforced by architecture, not policy.

---

## 5. Performance

| Metric | Value |
|--------|-------|
| microVM boot time | ~200ms (warm cache) |
| Policy evaluation (Wonk) | <1ms per check |
| Network proxy overhead (Goalie) | <5ms per request |
| Secret detection (Narc) | <1ms per tool call |
| Max concurrent operators | 3 per host (configurable) |
| Memory per operator VM | 4GB default |

---

## 6. Comparison

| Feature | Claude Code | Cursor | Smooth |
|---------|------------|--------|--------|
| Hardware isolation | No | No | **Yes (microVMs)** |
| Network filtering | No | No | **Yes (Goalie proxy)** |
| Filesystem deny patterns | No | No | **Yes (Wonk policy)** |
| Independent audit logging | No | No | **Yes (Scribe → Archivist)** |
| Enterprise policy override | No | No | **Yes (TOML deny rules)** |
| Multi-agent orchestration | No | No | **Yes (delegation API)** |
| Port forwarding (dev servers) | N/A | N/A | **Yes (policy-controlled)** |

---

## 7. Conclusion

AI agents will increasingly operate autonomously on production systems. The current model of "trust the agent, add guardrails after" is backwards. Smooth inverts this: **deny by default, allow by policy, enforce by hardware**.

Every tool call passes through Narc (detection) → Wonk (policy) → Goalie (network). Every file write is checked against mount-aware deny patterns. Every action is logged to a tamper-resistant audit trail in a separate VM.

This is not theoretical. Smooth ships as a single binary. The security architecture runs on every developer's laptop today.

---

*For more information, visit [github.com/SmooAI/smooth](https://github.com/SmooAI/smooth) or contact brent@smooai.com.*
