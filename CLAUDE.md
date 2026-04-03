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
│   ├── smooth-tui/          # Library — ratatui terminal dashboard
│   └── smooth-web/          # Library — embedded Vite SPA via rust-embed
│       └── web/             # React + Vite source (TypeScript)
├── Cargo.toml               # Workspace root
├── rustfmt.toml             # Format: 160 width, field init shorthand
├── install.sh               # Curl installer
├── .beads/                  # Issue tracking (Jira-synced)
└── .claude/hooks/           # Worktree enforcement
```

### Key Crates

- **smooth-cli** (`crates/smooth-cli/src/main.rs`): clap entry point, all command handlers
- **smooth-bigsmooth** (`crates/smooth-bigsmooth/src/`): axum server, 20+ routes, orchestrator state machine, sandbox pool, tool registry, policy generation, beads/jira/tailscale clients, audit logging
- **smooth-policy** (`crates/smooth-policy/src/`): shared policy types (network, filesystem, beads, tools, MCP), TOML parsing, glob matching
- **smooth-wonk** (`crates/smooth-wonk/src/`): in-VM access control authority, policy hot-reload, access negotiation with Big Smooth
- **smooth-goalie** (`crates/smooth-goalie/src/`): in-VM HTTP/HTTPS forward proxy + FUSE filesystem proxy, delegates all decisions to Wonk
- **smooth-narc** (`crates/smooth-narc/src/`): in-VM tool surveillance, prompt injection guard, regex pre-filters + LLM-as-a-judge
- **smooth-tui** (`crates/smooth-tui/src/`): ratatui app, views (dashboard, chat), markdown renderer, theme
- **smooth-web** (`crates/smooth-web/`): rust-embed serves compiled Vite SPA

---

## 2. Build, Test, Format, Lint

```bash
cargo build                  # Build all crates
cargo test                   # Run all tests (35 passing)
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
| `sandbox.rs` | msb CLI wrapper: create, destroy, exec, status; token gen, policy mount |
| `pool.rs` | Sandbox capacity (max 3), port allocation |
| `tools.rs` | Tool registry + hooks (secret detection, prompt injection) |
| `policy.rs` | Policy generation, phase defaults, access request handling |
| `beads.rs` | bd CLI wrapper (list, create, update, close, comment) |
| `chat.rs` | OpenCode Zen API (streaming + non-streaming) |
| `search.rs` | @ autocomplete (beads + globwalk files + path expansion) |
| `audit.rs` | Rotating file appender at ~/.smooth/audit/ |
| `db.rs` | rusqlite: memories, worker_runs, config tables |
| `jira.rs` | Jira REST client + bidirectional sync |
| `tailscale.rs` | tailscale CLI status wrapper |
| `ws.rs` | WebSocket message types |

### Security Architecture

The sandbox access control system uses named services running inside each microVM:

- **Big Smooth** — READ-ONLY orchestrator in "The Boardroom" VM
- **Archivist** — central log aggregator (can write only to log paths)
- **Wonk** — per-VM access control authority (rule engine, no LLM)
- **Goalie** — per-VM network + FUSE filesystem proxy (iptables enforced)
- **Narc** — per-VM tool surveillance + prompt injection guard (regex + LLM judge)
- **Scribe** — per-VM structured logging, feeds Archivist

See README.md for full architecture diagrams and the plan file for implementation details.

---

## 5. Data

All state at `~/.smooth/`:
- `smooth.db` — SQLite (WAL mode)
- `.beads/` — Beads issue graph
- `audit/` — Rotating tool usage logs per actor
- `providers.json` — LLM credentials
- `config.json` — CLI settings

---

## 6. Git Workflow

Same as smooai: worktrees, SMOODEV-XX branch naming, beads + Jira sync.

```bash
git worktree add ../smooth-SMOODEV-XX-desc -b SMOODEV-XX-desc main
# work in worktree
git checkout main && git pull --rebase && git merge SMOODEV-XX-desc --no-ff && git push
```

---

## 7. Testing — MANDATORY

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

## 8. Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

### Mandatory checklist

1. **Run quality gates** (if code changed):

    ```bash
    cargo fmt -- --check
    cargo clippy
    cargo test
    cargo build
    ```

2. **Close beads issues** for completed work:

    ```bash
    bd close <id1> <id2> ...
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
