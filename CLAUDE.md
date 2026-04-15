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
- **smooth-bigsmooth** (`crates/smooth-bigsmooth/`): axum server, 20+ routes, orchestrator, sandbox pool, policy generation, session management, pearls/jira/tailscale
- **smooth-operator** (`crates/smooth-operator/`): Rust-native AI agent framework — LLM client, tool system with hooks, agent loop, conversation management, built-in checkpointing (Groove)
- **smooth-policy** (`crates/smooth-policy/`): shared policy types (network, filesystem, pearls, tools, MCP), TOML parsing, glob matching, phase defaults
- **smooth-pearls** (`crates/smooth-pearls/`): built-in pearl tracker (dependency-graph work items). Dolt-backed via `smooth-dolt` Go binary for version control and git sync. Types: `Pearl`, `PearlStore`, `PearlStatus`, `PearlUpdate`, `PearlQuery`, `SmoothDolt`, `Registry`. Also stores session messages, orchestrator snapshots, and memories.
- **smooth-wonk** (`crates/smooth-wonk/`): in-VM access control authority, policy hot-reload via notify+ArcSwap, access negotiation with Big Smooth
- **smooth-goalie** (`crates/smooth-goalie/`): in-VM HTTP/HTTPS forward proxy, delegates all decisions to Wonk, JSON-lines audit logging
- **smooth-narc** (`crates/smooth-narc/`): tool surveillance via ToolHook, secret detection (10 patterns), prompt injection guard (6 patterns), write guard, severity-based alerts
- **smooth-operator-runner** (`crates/smooth-operator-runner/`): Binary that runs *inside* each microVM. Hosts the agent loop + file/bash tools + NarcHook, streams JSON-lines `AgentEvent`s on stdout. Cross-compiled to `aarch64-unknown-linux-musl`; Big Smooth mounts it into the sandbox at runtime. Build with `scripts/build-operator-runner.sh`.
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
pnpm install:th              # Install latest th to ~/.cargo/bin/
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
| `pearls.rs` | `PearlStore` wrapper (list, create, update, close, comment) |
| `search.rs` | @ autocomplete (pearls + globwalk files + path expansion) |
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
  the embedded `microsandbox` crate, mounts the cross-compiled
  `smooth-operator-runner` binary at `/opt/smooth/bin`, bind-mounts the
  user's working directory at `/workspace`, and execs the runner inside the
  VM. The runner hosts the agent loop, NarcHook tool surveillance, and file
  tools; it streams `AgentEvent`s as JSON-lines on stdout, which Big Smooth
  parses and forwards to WebSocket clients. Big Smooth performs zero writes,
  zero tool execution, and zero LLM calls — it is strictly the READ-ONLY
  orchestrator the security architecture promises.

The sandboxed path requires a one-time dev setup to build the runner
binary for the sandbox's target triple. On a fresh clone:

```bash
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild
pip3 install ziglang                          # provides python-zig for cargo-zigbuild
bash scripts/build-operator-runner.sh         # produces target/aarch64-unknown-linux-musl/release/smooth-operator-runner
```

Re-run `scripts/build-operator-runner.sh` after changing anything under
`crates/smooth-operator-runner/` or its transitive deps.

The in-process path is kept for backwards compatibility and for the existing
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

### Per-project (Dolt)
Pearl data lives in `.smooth/dolt/` per project, backed by an embedded
Dolt database (via the `smooth-dolt` Go binary). This gives full version
control, git-syncable data, and push/pull to remotes.

```
.smooth/dolt/          # Dolt database (content-addressed, git-friendly)
  └── pearls/          # Dolt "pearls" database
```

Tables: `pearls`, `pearl_dependencies`, `pearl_labels`, `pearl_comments`,
`pearl_history`, `sessions`, `session_messages`, `orchestrator_snapshots`,
`memories`.

### Global (`~/.smooth/`)
- `registry.json` — Multi-project registry (auto-updated on pearl store open)
- `smooth.db` — Legacy SQLite (migrate with `th pearls migrate-from-sqlite`)
- `audit/` — Rotating tool usage logs per actor
- `providers.json` — LLM credentials
- `project-cache/` — Project-scoped operator VM cache (keyed by workspace path hash). Bound into the sandbox at `/opt/smooth/cache` so repeated runs on the same repo share `pnpm install` / `cargo fetch` state. Manage via `th cache list|prune|clear`.
- `mcp.toml` — MCP server configs (see `docs/extending.md`)
- `plugins/<name>/plugin.toml` — CLI-wrapper tool manifests

### Project-scoped (`<repo>/.smooth/`)
- `dolt/` — Pearl database (see above)
- `mcp.toml` — Project-specific MCP servers; merged with global,
  project wins on name collision
- `plugins/<name>/plugin.toml` — Project-specific plugins; same
  merge rules

### Building smooth-dolt

```bash
# Requires Go 1.21+, ICU (macOS: brew install icu4c)
scripts/build-smooth-dolt.sh
# Produces target/release/smooth-dolt (~145MB, embedded Dolt engine)
```

---

## 6. Pearl Tracking — Dolt-backed + Jira Integration

**Philosophy**: Built-in pearl tracking (`th pearls`) is the primary work
tracker. Backed by embedded Dolt for version control and team sync.
Jira (SMOODEV project) is the external source of truth for project management.

**Pearls is the only spelling.** There are no `th issues` or `th beads`
aliases.

**Storage**: Dolt-only. No SQLite fallback. Each project has its own
`.smooth/dolt/` database. `~/.smooth/registry.json` tracks all projects.

**Naming lineage**: beads → issues → pearls.

### Quick reference

```bash
th pearls init                        # Initialize .smooth/dolt/ in current repo
th pearls create --title="Title" --description="..."
th pearls list --status=open          # All open pearls
th pearls list --status=in_progress   # Active work
th pearls show <id>                   # Pearl details with dependencies
th pearls update <id> --status=in_progress   # Claim work
th pearls close <id1> <id2> ...       # Close completed pearls
th pearls ready                       # Show ready pearls (open, no blockers)
th pearls blocked                     # Show blocked pearls
th pearls log                         # Dolt commit history
th pearls push                        # Push to Dolt remote
th pearls pull                        # Pull from Dolt remote
th pearls projects                    # List all registered pearl projects
th pearls migrate-from-sqlite         # Migrate legacy SQLite data to Dolt
th pearls migrate-from-beads          # Migrate from beads (bd CLI)
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

## 9. Changesets & Versioning

Always add changesets when landing work — this is how versions get bumped and changelogs generated.

```bash
pnpm changeset        # Interactive changeset creation
```

- Config: `.changeset/config.json`
- `package.json` is the single source of truth for the version
- `scripts/sync-versions.mjs` propagates the version to `Cargo.toml` workspace.package.version and `Cargo.lock`
- Release automated via GitHub Actions (`release.yml`) — Changesets PR → auto-merge → multi-platform binary build → GitHub Release
- Changesets describe what changed and why for the changelog

---

## 10. Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

### Mandatory checklist

1. **Run quality gates** (if code changed):

    ```bash
    cargo fmt -- --check
    cargo clippy
    cargo test
    cargo build
    pnpm install:th    # Update ~/.cargo/bin/th to latest
    ```

2. **Add changeset** for version bump:

    ```bash
    pnpm changeset    # Describe what changed and why
    ```

3. **Close pearls** for completed work:

    ```bash
    th pearls close <id1> <id2> ...
    ```

4. **Merge to main** if on feature branch:

    ```bash
    cd ~/dev/smooai/smooth
    git checkout main && git pull --rebase
    git merge <branch> --no-ff
    ```

5. **Push to remote**:

    ```bash
    git push
    git status  # MUST show "up to date with origin"
    ```

6. **Clean up** — remove worktrees, delete merged branches

7. **Verify** — all changes committed AND pushed

### Critical rules

- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing — that leaves work stranded locally
- NEVER say "ready to push when you are" — YOU must push
- All tests, clippy, and format checks must pass
- If push fails, resolve and retry until it succeeds
