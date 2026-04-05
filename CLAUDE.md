# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

**Use Context7 MCP server for up-to-date library documentation.**

> **CRITICAL: All feature work MUST happen in a git worktree.** Never edit source code or commit directly on `main` in `~/dev/smooai/smooth/`. A `PreToolUse` hook enforces this.

## Project Overview

Smooth is the Smoo AI CLI and orchestration platform — a **single Rust binary** (`th`) that coordinates Smooth Operators (AI agents in Microsandbox microVMs). Zero runtime dependencies.

---

## 1. Workspace Structure

```
smooth/
├── crates/
│   ├── smooth-cli/          # Binary — clap CLI (23 commands)
│   ├── smooth-bigsmooth/    # Library — orchestrator, policy generation, sandbox
│   ├── smooth-policy/       # Library — shared policy types, TOML parsing
│   ├── smooth-wonk/         # Binary — in-VM access control authority
│   ├── smooth-goalie/       # Binary — in-VM network + filesystem proxy
│   ├── smooth-narc/         # Binary — in-VM tool surveillance + LLM judge
│   ├── smooth-code/         # Library — ratatui terminal dashboard
│   └── smooth-web/          # Library — embedded Vite SPA via rust-embed
│       └── web/             # React + Vite source (TypeScript)
├── Cargo.toml               # Workspace root
├── rustfmt.toml             # Format: 160 width, field init shorthand
├── install.sh               # Curl installer
└── .claude/hooks/           # Worktree enforcement
```

### Key Crates

- **smooth-cli** (`crates/smooth-cli/`): clap entry point, 27 commands including `th access` for policy control
- **smooth-bigsmooth** (`crates/smooth-bigsmooth/`): axum server, 20+ routes, orchestrator, sandbox pool, policy generation, session management, issues/jira/tailscale
- **smooth-operator** (`crates/smooth-operator/`): Rust-native AI agent framework — LLM client, tool system with hooks, agent loop, conversation management, built-in checkpointing (Groove)
- **smooth-policy** (`crates/smooth-policy/`): shared policy types (network, filesystem, issues, tools, MCP), TOML parsing, glob matching, phase defaults
- **smooth-wonk** (`crates/smooth-wonk/`): in-VM access control authority, policy hot-reload via notify+ArcSwap, access negotiation with Big Smooth
- **smooth-goalie** (`crates/smooth-goalie/`): in-VM HTTP/HTTPS forward proxy, delegates all decisions to Wonk, JSON-lines audit logging
- **smooth-narc** (`crates/smooth-narc/`): tool surveillance via ToolHook, secret detection (10 patterns), prompt injection guard (6 patterns), write guard, severity-based alerts
- **smooth-scribe** (`crates/smooth-scribe/`): per-VM structured logging service, LogEntry with trace context, query/filter support
- **smooth-archivist** (`crates/smooth-archivist/`): central log aggregator, batch ingest from all Scribes, cross-VM query, stats, SSE event stream
- **smooth-code** (`crates/smooth-code/`): ratatui AI coding TUI — streaming chat, tool calls, file browser, git, sessions, model picker, extensions
- **smooth-web** (`crates/smooth-web/`): rust-embed serves compiled Vite SPA

---

## 2. Build, Test, Format, Lint

```bash
cargo build                  # Build all crates
cargo test                   # Run all tests (200+ across 10 crates)
cargo fmt                    # Format (rustfmt.toml: 160 width)
cargo clippy                 # Lint (pedantic + nursery)
cargo build --release -p smooth-cli  # Release binary (~10MB)
```

### Web UI (crates/smooth-web/web/)

```bash
cd crates/smooth-web/web
pnpm install
pnpm build                   # Builds to dist/, embedded in binary
pnpm dev                     # Vite dev server at :3100
```

---

## 3. Coding Style

### Rust
- Edition 2021, max_width 160, field init shorthand
- `unsafe_code = "forbid"`, `unused_must_use = "deny"`
- clippy pedantic + nursery (warn)
- `anyhow` for errors, `thiserror` for library errors
- `tracing` for logging

### Web (TypeScript/React)
- Vite + React 19 + Tailwind CSS 4
- oxfmt for formatting, oxlint for linting

---

## 4. Key Modules (smooth-bigsmooth)

| Module | Purpose |
|---|---|
| `server.rs` | axum router, all API routes (20+), access control routes |
| `orchestrator.rs` | State machine: Idle → Scheduling → Dispatching → Monitoring → Reviewing |
| `sandbox.rs` | Embedded [`microsandbox`] Rust SDK: create, destroy, exec, status. No external `msb` CLI — hardware-isolated microVMs boot directly from the binary. |
| `pool.rs` | Sandbox capacity (max 3), port allocation |
| `tools.rs` | Tool registry + hooks (secret detection, prompt injection) |
| `policy.rs` | Policy generation, phase defaults, access request handling |
| `issues.rs` | IssueStore wrapper (list, create, update, close, comment) |
| `chat.rs` | **DEPRECATED** — legacy OpenCode Zen API (use smooth-operator ProviderRegistry) |
| `search.rs` | @ autocomplete (issues + globwalk files + path expansion) |
| `audit.rs` | Rotating file appender at ~/.smooth/audit/ |
| `db.rs` | rusqlite: memories, worker_runs, config tables |
| `jira.rs` | Jira REST client + bidirectional sync |
| `tailscale.rs` | tailscale CLI status wrapper |
| `session.rs` | Session persistence, message history, orchestrator snapshots, inbox |
| `ws.rs` | WebSocket message types |

### Dispatch modes

Big Smooth's WebSocket `TaskStart` handler can dispatch tasks one of two ways:

- **In-process** (default): the agent loop runs inside Big Smooth's own process
  with tools executing against the host filesystem. Fast, works without any
  special setup, but Big Smooth is NOT read-only on this path.
- **Sandboxed** (`SMOOTH_SANDBOXED=1`): Big Smooth spawns a real microVM via
  the embedded `microsandbox` crate, runs the task inside the VM, and streams
  events back over the broadcast channel. Big Smooth performs zero writes and
  zero tool execution — it is strictly the READ-ONLY orchestrator the
  security architecture promises.

The sandboxed path is the one the architecture actually describes. The
in-process path is kept for backwards compatibility and for the existing
headless E2E tests. New features should target the sandboxed path.

### Security Architecture

The sandbox access control system uses named services running inside each microVM:

- **Big Smooth** — READ-ONLY orchestrator in "The Boardroom" VM
- **Archivist** — central log aggregator (can write only to log paths)
- **Wonk** — per-VM access control authority (rule engine, no LLM)
- **Goalie** — per-VM network + FUSE filesystem proxy (iptables enforced)
- **Narc** — per-VM tool surveillance + prompt injection guard (regex + LLM judge)
- **Scribe** — per-VM structured logging, feeds Archivist
- **Groove** — LLM checkpointing + session resume (built into smooth-operator)

See README.md for full architecture diagrams and the plan file for implementation details.

### smooth-operator (Agent Framework)

| Module | Purpose |
|---|---|
| `agent.rs` | Observe → think → act loop, event emission, checkpoint integration |
| `llm.rs` | OpenAI-compatible chat completion client, streaming-ready |
| `tool.rs` | Tool trait + ToolRegistry with pre/post hooks (Narc integration) |
| `conversation.rs` | Message history, context window management, token estimation |
| `checkpoint.rs` | Checkpoint + CheckpointStore trait, configurable strategies |

---

## 5. Data

All state at `~/.smooth/`:
- `smooth.db` — SQLite (WAL mode)
- `issues.db` — Built-in issue tracking (in smooth.db)
- `audit/` — Rotating tool usage logs per actor
- `providers.json` — LLM credentials
- `config.json` — CLI settings

---

## 6. Issue Tracking — Built-in + Jira Integration

**Philosophy**: Built-in issue tracking (`th issues`) replaces beads. Jira (SMOODEV project) is the external source of truth for project management.

### Quick reference

```bash
th issues create --title="SMOODEV-XX: Title" --description="..."
th issues list --status=open          # All open issues
th issues list --status=in_progress   # Active work
th issues show <id>                   # Issue details with dependencies
th issues update <id> --status=in_progress   # Claim work
th issues close <id1> <id2> ...       # Close completed issues
th issues ready                       # Show ready issues (open, no blockers)
th issues blocked                     # Show blocked issues
th issues migrate-from-beads          # One-time migration from beads (requires bd CLI)
```

---

## 7. Git Workflow

> **CRITICAL: All feature work MUST happen in a worktree.** Use `th worktree` commands.

```bash
# Create worktree for feature work
th worktree create SMOODEV-XX-desc

# List active worktrees
th worktree list

# When done: merge to main
th worktree merge SMOODEV-XX-desc

# Clean up
th worktree remove SMOODEV-XX-desc
```

Never edit source code or commit directly on `main`. Always use worktrees.

---

## 8. Testing — MANDATORY

> **CRITICAL: Every crate, every module, every public function MUST have tests.** No code lands without passing tests. This is non-negotiable.

- Tests colocated in each module (`#[cfg(test)]`)
- `cargo test` runs all — **must pass before any commit**
- `cargo clippy` must be clean (zero warnings) before commit
- `cargo fmt -- --check` must pass before commit
- Test categories:
  - **Unit tests**: every public function, every error path, every edge case
  - **Integration tests**: cross-module interactions (e.g., policy → sandbox, wonk → goalie)
  - **Property tests**: where applicable (e.g., policy round-trip serialization)
- When adding a new module: write tests FIRST or alongside, never "add tests later"
- When fixing a bug: add a regression test that fails without the fix
- Security-critical code (policy enforcement, access control, secret detection) requires **exhaustive** test coverage including adversarial inputs

---

## 9. Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

### Mandatory checklist

1. **Run quality gates** (if code changed):

    ```bash
    cargo fmt -- --check
    cargo clippy
    cargo test
    cargo build
    ```

2. **Close issues** for completed work:

    ```bash
    th issues close <id1> <id2> ...
    ```

3. **Merge to main** if on feature branch:

    ```bash
    cd ~/dev/smooai/smooth
    git checkout main && git pull --rebase
    git merge <branch> --no-ff
    ```

4. **Push to remote**:

    ```bash
    git push
    git status  # MUST show "up to date with origin"
    ```

5. **Clean up** — remove worktrees, delete merged branches

6. **Verify** — all changes committed AND pushed

### Critical rules

- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing — that leaves work stranded locally
- NEVER say "ready to push when you are" — YOU must push
- All tests, clippy, and format checks must pass
- If push fails, resolve and retry until it succeeds
