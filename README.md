<div align="center">

<img src="images/logo.png" alt="Smoo AI" width="120" />

# Smooth

**The Smoo AI CLI — Agent Orchestration & Platform Tools**

Coordinate teams of AI agents to build, research, analyze, and ship.
One binary for everything Smoo AI.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Release](https://img.shields.io/github/v/release/SmooAI/smooth?label=latest)](https://github.com/SmooAI/smooth/releases)

</div>

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh
```

Or build from source:

```bash
git clone https://github.com/SmooAI/smooth.git
cd smooth
cargo install --path crates/smooth-cli
```

## Quick Start

```bash
# Authenticate with your LLM provider
th auth login opencode-zen

# Start Smooth (leader API + embedded web dashboard)
th up

# Open the terminal UI
th tui
```

No Docker. No Node.js. No runtime dependencies. One 10MB binary.

---

## What is Smooth?

Smooth is the central CLI and orchestration platform for [Smoo AI](https://smoo.ai). It does two things:

1. **Agent Orchestration** — Spin up teams of AI agents (Smooth Operators) that work on real projects inside hardware-isolated Microsandbox microVMs. They assess, plan, execute, and review work autonomously with adversarial security review.

2. **Smoo AI Platform CLI** — Manage config schemas, interact with the SmooAI API, sync with Jira, and control your infrastructure from one command.

### How it works

```
ASSESS → PLAN → ORCHESTRATE → EXECUTE → FINALIZE → REVIEW (adversarial)
```

Every piece of work gets adversarial review from a separate operator that challenges assumptions, checks for security issues, and either approves, requests rework, or rejects. All state is durable through [Beads](https://github.com/SmooAI/beads).

---

## Architecture

```mermaid
graph TB
    subgraph Host["Host Machine"]
        MSB["msb server<br/><small>Microsandbox daemon</small>"]
    end

    subgraph Boardroom["The Boardroom (microsandbox)"]
        BS["Big Smooth<br/><small>orchestrator, READ-ONLY</small>"]
        AR["Archivist<br/><small>central log aggregator</small>"]
        BW["Wonk<br/><small>Boardroom access control</small>"]
        BG["Goalie<br/><small>Boardroom network proxy</small>"]
        BN["Narc<br/><small>blocks Big Smooth writes</small>"]
        BSc["Scribe<br/><small>Boardroom logging</small>"]
    end

    subgraph Op1["Operator VM 1 (microsandbox)"]
        OC1["OpenCode<br/><small>AI agent</small>"]
        W1["Wonk<br/><small>access control</small>"]
        G1["Goalie<br/><small>network + fs proxy</small>"]
        N1["Narc<br/><small>tool surveillance</small>"]
        S1["Scribe<br/><small>structured logging</small>"]
    end

    subgraph Op2["Operator VM 2 (microsandbox)"]
        OC2["OpenCode<br/><small>AI agent</small>"]
        W2["Wonk"] & G2["Goalie"] & N2["Narc"] & S2["Scribe"]
    end

    MSB --> Boardroom
    MSB --> Op1
    MSB --> Op2
    BS -->|orchestrates| Op1
    BS -->|orchestrates| Op2
    S1 -->|events| AR
    S2 -->|events| AR

    style Host fill:#020618,stroke:#30363d,color:#f8fafc
    style Boardroom fill:#040d30,stroke:#f49f0a,color:#f8fafc
    style Op1 fill:#040d30,stroke:#22c55e,color:#f8fafc
    style Op2 fill:#040d30,stroke:#22c55e,color:#f8fafc
```

### The Cast

Everything runs inside [Microsandbox](https://github.com/nicholasgasior/microsandbox) microVMs — including the orchestrator.

| Service | Role | Where it runs |
|---|---|---|
| **Big Smooth** | Orchestrator. Schedules work, generates policies, handles access requests. **READ-ONLY** — cannot write to the filesystem. | The Boardroom |
| **Archivist** | Central log + trace aggregator. Receives events and OTLP traces from all Scribes. Stores traces in SQLite, optionally forwards to external OTel backends (Jaeger, Tempo, Honeycomb). Can write, but only to log paths. | The Boardroom |
| **Wonk** | Access control authority. Reads policy TOML, answers "is this allowed?" for every network request, tool call, bead access, and CLI command. No LLM. | Every VM |
| **Goalie** | Network + filesystem proxy. Dumb pipe — forwards or blocks based on Wonk's answer. iptables + FUSE enforced at kernel level. | Every VM |
| **Narc** | Tool surveillance + prompt injection guard. Two-tier detection: fast regex pre-filters + LLM-as-a-judge for ambiguous cases. | Every VM |
| **Scribe** | Structured logging service. All services log through Scribe, which writes to on-pod SQLite and feeds Archivist. | Every VM |
| **Groove** | LLM checkpointing + session resume. Captures conversation state after tool calls and phase transitions. Enables interrupted operators to resume from last checkpoint. | Every VM |

**The Board** = Big Smooth + Archivist (leadership). **The Boardroom** = the VM where The Board operates, with its own Wonk, Goalie, Narc, Scribe, and Groove.

**Smooth Operators** = the AI agents. The only ones who write code.

### Inside each MicroVM

```mermaid
graph LR
    subgraph VM["MicroVM (--scope none)"]
        Operator["Operator / Big Smooth"]
        Wonk["Wonk<br/><small>:8400</small>"]
        Goalie["Goalie<br/><small>:8480 proxy</small>"]
        Narc["Narc"]
        Scribe["Scribe<br/><small>:8401</small>"]
    end

    Operator -->|HTTP_PROXY| Goalie
    Goalie -->|"is this allowed?"| Wonk
    Narc -->|intercepts| Operator
    Narc -->|"check tool"| Wonk
    Operator --> Scribe
    Wonk --> Scribe
    Goalie --> Scribe
    Narc --> Scribe

    style VM fill:#040d30,stroke:#0a1f7a,color:#f8fafc
```

- **Wonk** reads `/etc/smooth/policy.toml`, listens on `127.0.0.1:8400`, hot-reloads on file change
- **Goalie** listens on `127.0.0.1:8480` as HTTP proxy. iptables rejects all outbound TCP except from the Goalie UID. FUSE mount at `/workspace` for filesystem access control.
- **Narc** intercepts tool calls and incoming prompts. Regex fast path catches obvious secrets and write violations. Ambiguous cases go to a small/fast LLM (Haiku, Flash, GPT-4o-mini) for a yes/no verdict.
- **Scribe** listens on `127.0.0.1:8401`, writes to on-pod SQLite and JSON-lines, feeds events to Archivist. Bridges `tracing` spans to OpenTelemetry via `tracing-opentelemetry`, generating trace hierarchies for operator lifecycles, prompts, tool calls, and network requests. Exports OTLP traces to Archivist with W3C traceparent propagation across VM boundaries.

### Security Model

```mermaid
graph TD
    subgraph Enforcement["Kernel-Level Enforcement"]
        IPT["iptables<br/><small>only Goalie UID can make outbound connections</small>"]
        FUSE["FUSE mount<br/><small>all file I/O goes through Goalie</small>"]
        SCOPE["--scope none<br/><small>microsandbox blocks direct internet</small>"]
    end

    subgraph Policy["Policy-Driven Access Control"]
        TOML["policy.toml<br/><small>generated by Big Smooth per operator</small>"]
        NET["Network allowlist<br/><small>domain + path matching</small>"]
        FS["Filesystem deny patterns<br/><small>*.env, *.pem, .ssh/*</small>"]
        TOOL["Tool allowlist<br/><small>per-phase tool access</small>"]
        BEAD["Bead scoping<br/><small>operator sees only assigned beads + deps</small>"]
        MCP["MCP server allowlist<br/><small>deny unknown servers by default</small>"]
    end

    subgraph Detection["Two-Tier Threat Detection (Narc)"]
        REGEX["Regex fast path<br/><small>secrets, write guard, known patterns</small>"]
        LLM["LLM judge<br/><small>Haiku/Flash for ambiguous cases</small>"]
    end

    TOML --> NET & FS & TOOL & BEAD & MCP
    IPT --> NET
    FUSE --> FS

    style Enforcement fill:#14532d,stroke:#22c55e,color:#f8fafc
    style Policy fill:#040d30,stroke:#0a1f7a,color:#f8fafc
    style Detection fill:#422006,stroke:#f49f0a,color:#f8fafc
```

**Key invariants:**
- Big Smooth **never writes**. Narc in the Boardroom enforces this — any write attempt is instantly blocked.
- Archivist **can write**, but only to log paths. Writes to any other path are blocked.
- Operators can only see their assigned beads and dependencies (scoped by auth token).
- All outbound traffic goes through Goalie. No process can bypass the proxy — enforced at the kernel level.

### Continuous Access Negotiation

Operators can request expanded access at runtime. The flow:

```mermaid
sequenceDiagram
    participant Op as Operator
    participant G as Goalie
    participant W as Wonk
    participant BS as Big Smooth

    Op->>G: GET api.stripe.com/v1/charges
    G->>W: is this allowed?
    W-->>G: BLOCKED (not in allowlist)
    G-->>Op: 403 Blocked
    G->>W: request access
    W->>BS: POST /api/access/request
    BS->>BS: auto-approve? check bead labels?
    alt auto-approved
        BS-->>W: approved + updated policy
        W-->>W: hot-reload policy
        Note over Op,G: retry succeeds
    else needs human
        BS->>BS: send to inbox
        Note over BS: th access approve <bead> <domain>
    end
```

### Operator Lifecycle

```mermaid
graph LR
    A["ASSESS"] --> P["PLAN"] --> O["ORCHESTRATE"] --> E["EXECUTE"] --> F["FINALIZE"] --> R["REVIEW"]
    R -->|approved| Done["Done"]
    R -->|rework| E
    R -->|"security\nfailed"| E

    style A fill:#040d30,stroke:#0a1f7a,color:#f8fafc
    style P fill:#040d30,stroke:#0a1f7a,color:#f8fafc
    style O fill:#040d30,stroke:#0a1f7a,color:#f8fafc
    style E fill:#14532d,stroke:#22c55e,color:#f8fafc
    style F fill:#040d30,stroke:#0a1f7a,color:#f8fafc
    style R fill:#422006,stroke:#f49f0a,color:#f8fafc
    style Done fill:#14532d,stroke:#22c55e,color:#f8fafc
```

### Phase-Based Access Defaults

| Phase | Network | Filesystem | Beads |
|---|---|---|---|
| Assess | LLM + registries | Read-only | Own bead + deps (depth 1) |
| Plan | LLM + registries | Read-only | Own bead + deps (depth 2) |
| Orchestrate | LLM + registries + leader | Read-only | Own bead + deps (depth 2) |
| Execute | LLM + registries + GitHub | Read-write | Own bead + deps (depth 2) |
| Finalize | LLM + registries + GitHub | Read-write | Own bead + deps (depth 2) |
| Review | LLM + registries | Read-only | Target bead + own bead |

---

## The `th` CLI

### Core

```bash
th up                            # Start everything
th down                          # Stop
th status                        # System health
th tui                           # Terminal UI (ratatui)
```

### Authentication

```bash
th auth login opencode-zen       # OpenCode Zen (Claude, GPT, Gemini, etc.)
th auth login anthropic          # Direct Anthropic API
th auth status                   # Show all auth status
th auth providers                # List configured providers
```

### Work

```bash
th run <bead-id>                 # Trigger work on a bead
th operators                     # List active Smooth Operators
th pause/resume/steer/cancel     # Control operators mid-task
th approve <bead-id>             # Approve a review
th inbox                         # Messages needing attention
```

### Access Control

```bash
th access pending                # List pending access requests
th access approve <bead> <domain>  # Approve domain access
th access deny <bead> <domain>     # Deny domain access
th access policy <operator-id>     # Show current policy
```

### System

```bash
th db status                     # Database info
th db backup                     # Backup SQLite
th audit tail leader             # View audit logs
th tailscale status              # Tailscale info
th worktree create/list/merge    # Git worktrees
```

---

## Tech Stack

| | |
|---|---|
| **Language** | Rust 2021 edition |
| **HTTP** | axum + tower |
| **Database** | rusqlite (bundled SQLite) |
| **TUI** | ratatui + crossterm |
| **Web** | React 19 + Vite + Tailwind CSS 4 (embedded) |
| **Markdown** | pulldown-cmark (TUI), react-markdown (web) |
| **Sandboxes** | Microsandbox (hardware-isolated microVMs) |
| **LLM** | OpenCode Zen API (OpenAI-compatible) |
| **Work tracking** | Beads (durable SoR) |
| **Policy** | TOML-based, hot-reloadable via notify + ArcSwap |
| **Logging** | smooai-logger (structured, context-aware) |
| **Tracing** | OpenTelemetry (tracing-opentelemetry bridge, OTLP export) |
| **Linting** | clippy (pedantic + nursery) |
| **Formatting** | rustfmt (160 max width) |

## Workspace

```
smooth/
├── crates/
│   ├── smooth-cli/          # Binary — clap CLI (23 commands)
│   ├── smooth-bigsmooth/    # Library — orchestrator, policy generation
│   ├── smooth-policy/       # Library — shared policy types, TOML parsing
│   ├── smooth-wonk/         # Binary — in-VM access control authority
│   ├── smooth-goalie/       # Binary — in-VM network + filesystem proxy
│   ├── smooth-narc/         # Binary — in-VM tool surveillance + LLM judge
│   ├── smooth-scribe/       # Binary — in-VM structured logging + OTel
│   ├── smooth-archivist/    # Binary — central log + trace aggregator
│   ├── smooth-groove/       # Binary — in-VM LLM checkpointing + session resume
│   ├── smooth-tui/          # Library — ratatui terminal dashboard
│   └── smooth-web/          # Library — embedded Vite SPA
│       └── web/             # React + Vite source
├── Cargo.toml               # Workspace root
├── rustfmt.toml             # Format config
└── install.sh               # Curl installer
```

## Development

```bash
# Build
cargo build

# Test (35 tests)
cargo test

# Format
cargo fmt

# Lint
cargo clippy

# Run dev (with auto-reload)
cargo watch -x 'run -p smooth-cli -- up'

# Release build (~10MB)
cargo build --release -p smooth-cli
ls -lh target/release/th
```

## License

MIT - [Smoo AI](https://smoo.ai)
