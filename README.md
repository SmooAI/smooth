<div align="center">

<img src="images/logo.png" alt="Smooth" width="420" />

# Smooth

**The Smoo AI CLI — Agent Orchestration & Platform Tools**

Coordinate teams of AI agents to build, research, analyze, and ship.
One binary for everything Smoo AI.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Release](https://img.shields.io/github/v/release/SmooAI/smooth?label=latest)](https://github.com/SmooAI/smooth/releases)

</div>

---

## About Smoo AI

**[Smoo AI](https://smoo.ai)** is an AI platform that helps businesses multiply their customer, employee, and developer experience — conversational AI for support and sales, paired with the production-grade developer tooling we use to build it.

Smooth is part of a small family of open-source packages we maintain to keep our own stack honest: contextual logging, typed HTTP, file storage, and agent orchestration. Use them in your stack, or take them as a reference for how we build.

- 🌐 [smoo.ai](https://smoo.ai) — the product
- 📦 [smoo.ai/open-source](https://smoo.ai/open-source) — every open-source package we ship
- 🐙 [github.com/SmooAI](https://github.com/SmooAI) — the source

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
# Authenticate with Smoo AI's gateway (resolves every smooth-* slot)
th auth login smooai-gateway

# Start Smooth (Big Smooth API + embedded web dashboard)
th up

# Open the interactive coding assistant
th code
```

Or bring your own provider — see [Authentication](#authentication)
below for the full list.

No Docker. No Node.js. No runtime dependencies. One 10MB binary.

---

## What is Smooth?

Smooth is the central CLI and orchestration platform for [Smoo AI](https://smoo.ai). It does two things:

1. **Agent Orchestration** — Dispatch Smooth Operators (AI agents) to work on real projects inside hardware-isolated Microsandbox microVMs, with adversarial surveillance and policy-gated access control.

2. **Smoo AI Platform CLI** — Manage config schemas, interact with the Smoo AI API, sync with Jira, and control your infrastructure from one command.

### How the agent loop works

Inside each operator VM, a **single agent** handles its own inner iteration
(LLM → tool → LLM → …) via `smooth-operator`'s agent loop. A thin outer
governor wraps it with three jobs: feed last run's test output back in,
snapshot the workspace when failing tests drop, and stop on the first
convincing signal.

```mermaid
%%{init: {"flowchart": {"defaultRenderer": "elk"}, "themeVariables": {"lineColor": "#f49f0a"}}}%%
flowchart LR
    START["Task prompt"]
    TURN["Coding turn<br/><small>smooth-coding · agent runs tools internally</small>"]
    GREEN{"Tests green?"}
    SNAP["Snapshot workspace<br/><small>if failing count dropped</small>"]
    STOP{"Stop signal?<br/><small>close-to-green · budget · iter cap</small>"}
    DONE["Done"]
    RESTORE["Restore best-seen state"]

    START --> TURN
    TURN --> GREEN
    GREEN -- yes --> DONE
    GREEN -- no --> SNAP
    SNAP --> STOP
    STOP -- no --> TURN
    STOP -- yes --> RESTORE
    RESTORE --> DONE

    style START fill:#0a1f7a,stroke:#18387a,color:#f8fafc
    style TURN fill:#040d30,stroke:#22c55e,color:#f8fafc
    style SNAP fill:#040d30,stroke:#f49f0a,color:#f8fafc
    style GREEN fill:#0a1f7a,stroke:#18387a,color:#f8fafc
    style STOP fill:#0a1f7a,stroke:#18387a,color:#f8fafc
    style RESTORE fill:#040d30,stroke:#f49f0a,color:#f8fafc
    style DONE fill:#14532d,stroke:#22c55e,color:#f8fafc
```

Implemented in [`smooth-operator::coding_workflow`](crates/smooth-operator/src/coding_workflow.rs).
An earlier version decomposed the run into seven phases (ASSESS / PLAN /
EXECUTE / VERIFY / REVIEW / TEST / FINALIZE). The phase pipeline kept
silently short-circuiting at one detector or another; the single-agent
loop is smaller, easier to reason about, and matches the shape of
benchmark-tuned coding agents. We kept the self-validation requirement
in the system prompt, the best-state snapshot, and the compile-error
short-circuit — and dropped per-phase dispatch.

**Stop conditions** are budget + plateau, not a fixed iteration cap:

- **Green** — agent reports all tests passing.
- **Close-to-green** — a previous turn reached ≤3 failing tests; this
  turn didn't improve on it. More iteration is more likely to regress.
- **Budget** — next turn would blow the `--budget-usd` cap.
- **Iteration cap** — safety ceiling (default 5), not the primary brake.

### Model routing

Every LLM call dispatches through a **semantic routing slot**. The gateway
(typically `llm.smoo.ai`) resolves each slot to a concrete model, so
upgrading backends doesn't churn the code.

| Slot | Used by | Shape |
|---|---|---|
| `smooth-coding` | The coding loop (workhorse) | Strong tool use + multi-turn |
| `smooth-thinking` | `th code` Thinking preset, deep reasoning | Extended chain-of-thought |
| `smooth-planning` | `th code` Planning preset | Task decomposition |
| `smooth-reviewing` | `th code` Reviewing preset, code-review flows | Adversarial critique |
| `smooth-judge` | Narc's LLM-as-a-judge, bench scoring | Yes/no verdicts, low latency |
| `smooth-summarize` | Context compression during long runs | Summarization |
| `smooth-fast` | Session auto-naming, short titles, autocomplete | Haiku/Flash-class, sub-second TTFT |
| `smooth-default` | Fallback when a specific slot isn't configured | Generalist |

Routing is in [`smooth-operator::providers`](crates/smooth-operator/src/providers.rs).
The CLI's `th code` presets remap slots to arbitrary models via the
model picker — e.g. point Coding at Kimi Code for a run, Thinking at
GLM, whatever.

**Live status.** The TUI streams an `AgentEvent::PhaseStart` on each
coding turn and shows iteration + routing alias + resolved upstream +
spend in the status bar:

```
CODING · smooth-coding → minimax-m2.7 | iter 3/5 | failed: 4 → 1 | spend: $0.012
```

All state is durable through Smooth's built-in pearl tracker (Dolt-backed
per-project, git-syncable).

---

## Architecture

```mermaid
%%{init: {"flowchart": {"defaultRenderer": "elk"}, "themeVariables": {"lineColor": "#f49f0a"}}}%%
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
        OC1["smooth-operator-runner<br/><small>agent + tools</small>"]
        W1["Wonk<br/><small>access control</small>"]
        G1["Goalie<br/><small>network + fs proxy</small>"]
        N1["Narc<br/><small>tool surveillance</small>"]
        S1["Scribe<br/><small>structured logging</small>"]
    end

    subgraph Op2["Operator VM 2 (microsandbox)"]
        OC2["smooth-operator-runner<br/><small>agent + tools</small>"]
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
| **Wonk** | Access control authority. Reads policy TOML, answers "is this allowed?" for every network request, tool call, pearl access, and CLI command. No LLM. | Every VM |
| **Goalie** | Network + filesystem proxy. Dumb pipe — forwards or blocks based on Wonk's answer. iptables + FUSE enforced at kernel level. | Every VM |
| **Narc** | Tool surveillance + prompt injection guard. Two-tier detection: fast regex pre-filters + LLM-as-a-judge for ambiguous cases. | Every VM |
| **Scribe** | Structured logging service. All services log through Scribe, which writes to on-pod SQLite and feeds Archivist. | Every VM |
| **Groove** | LLM checkpointing + session resume. Captures conversation state after tool calls. Enables interrupted operators to resume from last checkpoint. | Every VM |

**The Board** = Big Smooth + Archivist (leadership). **The Boardroom** = the VM where The Board operates, with its own Wonk, Goalie, Narc, Scribe, and Groove.

**Smooth Operators** = the AI agents. The only ones who write code.

### Inside each MicroVM

```mermaid
%%{init: {"flowchart": {"defaultRenderer": "elk"}, "themeVariables": {"lineColor": "#f49f0a"}}}%%
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
        TOOL["Tool allowlist<br/><small>per-operator tool access</small>"]
        BEAD["Pearl scoping<br/><small>operator sees only assigned pearls + deps</small>"]
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
- Operators can only see their assigned pearls and dependencies (scoped by auth token).
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
    BS->>BS: auto-approve? check pearl labels?
    alt auto-approved
        BS-->>W: approved + updated policy
        W-->>W: hot-reload policy
        Note over Op,G: retry succeeds
    else needs human
        BS->>BS: send to inbox
        Note over BS: th access approve <pearl> <domain>
    end
```

### Default access envelope

Each operator VM boots with a minimal envelope:

- **Network**: the configured LLM gateway (`llm.smoo.ai` by default),
  relevant package registries (crates.io, npm, PyPI), and GitHub. Any
  other domain needs explicit approval — see continuous access negotiation
  above.
- **Filesystem**: read-write on `/workspace` (bind-mount of the user's
  repo). Everything else is read-only or denied. `.env`, `*.pem`,
  `.ssh/*`, and other secret-shaped paths are always denied.
- **Pearls**: the assigned pearl + its dependency closure (depth 2).
  Tasks cannot reach pearls outside that closure.
- **Tools**: the registered tool allowlist (file read/write, bash via
  Goalie, MCP tools that were approved, CLI-wrapper plugins). Every
  invocation passes Narc's regex prefilter + ambiguous-case LLM judge.

---

## The `th` CLI

### Core

```bash
th up                            # Start everything
th down                          # Stop
th status                        # System health
th code                          # Interactive coding assistant (ratatui)
```

### Authentication

Smooth talks to any OpenAI-compatible endpoint. The recommended default
is **[llm.smoo.ai](https://llm.smoo.ai)** — our LiteLLM-backed gateway
that maps every `smooth-*` routing slot to a production-tuned upstream
(Claude, GPT, Gemini, Kimi, MiniMax, GLM, Qwen, etc.) with Stripe-
metered billing, org/team keys, and an admin dashboard. One key, every
model, no per-provider plumbing.

```bash
# Smoo AI's gateway (recommended — every slot resolves via one key)
th auth login smooai-gateway

# Or bring your own upstream — any OpenAI-compatible provider:
th auth login kimi-code          # Moonshot Kimi Code (coding workhorse)
th auth login kimi               # Moonshot Kimi chat endpoint
th auth login openrouter         # OpenRouter (aggregator over many providers)
th auth login openai             # OpenAI direct
th auth login anthropic          # Anthropic direct
th auth login google             # Google (Gemini)
th auth login ollama             # Local Ollama models

th auth status                   # Show all auth status
th auth providers                # List configured providers
th auth default <provider>       # Which provider backs smooth-default
```

Providers and slots are independent: you can pin each routing slot
(`smooth-coding`, `smooth-thinking`, …) to a different provider/model
via `th code`'s model picker or by editing `~/.smooth/providers.json`.

### Work

```bash
th run <pearl-id>                # Trigger work on a pearl
th operators                     # List active Smooth Operators
th pause/resume/steer/cancel     # Control operators mid-task
th approve <pearl-id>            # Approve a review
th inbox                         # Messages needing attention
```

### Access Control

```bash
th access pending                   # List pending access requests
th access approve <pearl> <domain>  # Approve domain access
th access deny <pearl> <domain>     # Deny domain access
th access policy <operator-id>      # Show current policy
```

### Tools & Plugins

```bash
# MCP servers (Playwright, GitHub, filesystem, etc.)
th mcp add playwright npx @playwright/mcp@latest
th mcp add --project repo-fs npx @modelcontextprotocol/server-filesystem /workspace
th mcp list                      # Global + project scopes
th mcp test playwright           # Health check
th mcp remove playwright

# CLI-wrapper plugins — shell commands exposed as agent tools
th plugin init jq --command 'jq {{filter}} <<< {{json}}'
th plugin init --project deploy --command 'scripts/deploy.sh {{env}}'
th plugin list
th plugin remove deploy --project
```

Global config lives at `~/.smooth/`; project config at
`<repo>/.smooth/`. Project entries shadow global on name collision.
See [`docs/extending.md`](docs/extending.md) for the full guide.

### Run a pearl in a sandbox (`th run`)

Dispatch a pearl (or ad-hoc prompt) to a Smooth Operator running in a
microVM. The agent has bind-mount access to your workspace, a
project-scoped cache at `/opt/smooth/cache`, and (with `--keep-alive`)
forwarded ports so you can review dev servers live.

```bash
# First ready pearl, default image
th run --keep-alive

# Explicit pearl, explicit memory
th run th-abcdef --keep-alive --memory-mb 6144

# Ad-hoc prompt against the current directory
th run "add a /health route that returns {\"ok\":true}" --keep-alive

# Inspect + tear down
th operators list
th operators kill <operator-id>
```

**One image for every stack.** `smooai/smooth-operator` ships with
alpine + `mise` baked in. The agent reads the workspace
(`package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`) and
installs whatever toolchain it needs at runtime — node + pnpm,
python + uv, rust, go, bun, deno, or any of the ~140 tools mise
supports. Installs land in `/opt/smooth/cache/mise`, bound to the
host project cache so second-run starts are offline-fast.

Build locally:

```bash
scripts/build-smooth-operator-image.sh
```

Override via `--image` or `SMOOTH_OPERATOR_IMAGE` env if you want a
custom variant (e.g. a version pinned for CI reproducibility).

**Microsandbox image resolution.** Locally-built images live in
your Docker Desktop image store; `microsandbox` pulls from registries
by default, so if its pull can't see your local build, push it
first (`docker push smooai/smooth-operator:0.2.0`) or set
`SMOOTH_WORKER_IMAGE` to something microsandbox can reach.

**Project cache.** Each workspace path hashes to its own cache,
mounted at `/opt/smooth/cache` inside the VM. Subsequent runs on the
same repo share mise installs + language stores (pnpm-store, cargo
registry, uv cache, etc.). Backed by a first-class microsandbox
Volume by default (`~/.microsandbox/volumes/smooth-cache-<key>/`);
set `SMOOTH_USE_VOLUMES=0` to fall back to the legacy bind-mount
(`~/.smooth/project-cache/<key>/`). Manage with:

```bash
th cache list                     # shows entries from both backends, tagged
th cache prune --older-than 30    # evict caches idle > N days
th cache clear /path/to/project   # remove entry for a specific workspace
```

### Background service

Keep `th up` running across reboots via the native service manager
(user-level; no sudo, no system daemons).

```bash
th service install               # LaunchAgent (macOS) / systemd --user (Linux) / logon task (Windows)
th service start / stop / restart
th service status
th service logs -f               # Tail ~/.smooth/service.log
th service uninstall
th service install --system      # Print the system-level artifact + install instructions
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

## Extending Smooth

Two extension points add tools without rebuilding the binary:

- **MCP servers** — spawn [Model Context
  Protocol](https://modelcontextprotocol.io) servers like Playwright
  MCP or GitHub MCP; their tools land in the agent's registry as
  `<server>.<tool>`.
- **CLI-wrapper plugins** — drop a TOML manifest at
  `.smooth/plugins/<name>/plugin.toml` and the runner registers it as
  `plugin.<name>`, rendering `{{placeholder}}` args into a shell
  command template.

Both are configurable globally (`~/.smooth/`) and per-project
(`<repo>/.smooth/`). Project entries shadow global ones. There's
**no trust gate** on loading these — consistent with `npm install`,
`.zshrc`, or cloning any repo and running `pnpm dev`. Defense-in-depth
happens at *call time*: Narc's CliGuard / injection / secret
detectors gate every tool invocation, Wonk policy gates every
network + filesystem access, and the whole agent loop runs inside
a hardware-isolated microVM. See [`docs/extending.md`](docs/extending.md)
and [`SECURITY.md`](SECURITY.md).

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
| **Agent framework** | smooth-operator (Rust-native, built-in checkpointing) |
| **LLM** | OpenAI-compatible via `llm.smoo.ai` gateway by default (Kimi, MiniMax, GLM, Qwen, Anthropic, OpenAI, Google) |
| **Work tracking** | Pearls (Dolt-backed, git-syncable) |
| **Policy** | TOML-based, hot-reloadable via notify + ArcSwap |
| **Logging** | smooai-logger (structured, context-aware) |
| **Tracing** | OpenTelemetry (tracing-opentelemetry bridge, OTLP export) |
| **Linting** | clippy (pedantic + nursery) |
| **Formatting** | rustfmt (160 max width) |

## Workspace

```
smooth/
├── crates/
│   ├── smooth-cli/               # Binary — clap CLI, the `th` entry point
│   ├── smooth-bigsmooth/         # Library — orchestrator, policy gen, session mgmt
│   ├── smooth-bootstrap-bill/    # Library + binary — host-side microsandbox broker ("Bill")
│   ├── smooth-operator/          # Library — Rust-native AI agent framework
│   ├── smooth-operator-runner/   # Binary — agent loop inside each operator VM
│   ├── smooth-policy/            # Library — shared policy types, TOML parsing
│   ├── smooth-wonk/              # Binary — in-VM access control authority
│   ├── smooth-goalie/            # Binary — in-VM network + filesystem proxy
│   ├── smooth-narc/              # Library — tool surveillance + secret detection
│   ├── smooth-scribe/            # Library — per-VM structured logging
│   ├── smooth-archivist/         # Library — central log aggregator
│   ├── smooth-pearls/            # Library — Dolt-backed pearl tracker
│   ├── smooth-plugin/            # Library — CLI-wrapper plugin manifests
│   ├── smooth-diver/             # Library — deep research / exploratory agent
│   ├── smooth-tunnel/            # Library — th.smoo.ai reverse-tunnel client
│   ├── smooth-bench/             # Binary — coding-benchmark harness (aider-polyglot, SWE-bench, …)
│   ├── smooth-code/              # Library — ratatui terminal dashboard
│   └── smooth-web/               # Library — embedded Vite SPA
│       └── web/                  # React + Vite source
├── Cargo.toml                    # Workspace root
├── rustfmt.toml                  # Format config
└── install.sh                    # Curl installer
```

## Development

```bash
# Build
cargo build

# Test (full suite across all crates)
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
