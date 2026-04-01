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
│   ├── smooth-cli/          # Binary crate — clap CLI (23 commands)
│   ├── smooth-leader/       # Library — axum server, orchestrator, sandbox, tools
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
- **smooth-leader** (`crates/smooth-leader/src/`): axum server, 20+ routes, orchestrator state machine, sandbox pool, tool registry, beads/jira/tailscale clients, audit logging
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

## 4. Key Modules (smooth-leader)

| Module | Purpose |
|---|---|
| `server.rs` | axum router, all API routes |
| `orchestrator.rs` | State machine: Idle → Scheduling → Dispatching → Monitoring → Reviewing |
| `sandbox.rs` | msb CLI wrapper: create, destroy, exec, status |
| `pool.rs` | Sandbox capacity (max 3), port allocation |
| `tools.rs` | Tool registry + hooks (secret detection, prompt injection) |
| `beads.rs` | bd CLI wrapper (list, create, update, close, comment) |
| `chat.rs` | OpenCode Zen API (streaming + non-streaming) |
| `search.rs` | @ autocomplete (beads + globwalk files + path expansion) |
| `audit.rs` | Rotating file appender at ~/.smooth/audit/ |
| `db.rs` | rusqlite: memories, worker_runs, config tables |
| `jira.rs` | Jira REST client + bidirectional sync |
| `tailscale.rs` | tailscale CLI status wrapper |
| `ws.rs` | WebSocket message types (Phase 4+) |

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

## 7. Testing

- Tests colocated in each module (`#[cfg(test)]`)
- `cargo test` runs all
- 35 tests: db, audit, search, beads, jira, tailscale, server, chat, orchestrator, sandbox, pool, tools, ws, markdown
