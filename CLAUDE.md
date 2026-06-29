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
│   ├── smooth-cli/          # Binary — the `th` clap CLI
│   ├── smooth-daemon/       # Binary — the always-on personal-agent daemon (hosts the operator)
│   ├── smooth-tools/        # Library — workspace-scoped agent tools (fs/grep/bash) + Gate-1 deny
│   ├── smooth-policy/       # Library — shared policy types + the Gate-1 auto-mode rule engine
│   ├── smooth-goalie/       # Library — egress allowlist proxy (the network boundary)
│   ├── smooth-cast/         # Library — coding-harness extensions to the operator (fixer/oracle/… roles)
│   ├── smooth-pearls/       # Library — built-in pearl tracker (Dolt-backed)
│   ├── smooth-diver/        # Library — pearl lifecycle + project-management service
│   ├── smooth-plugin/       # Library — trait-based plugin system (CLI/API/TUI/tool extensions)
│   ├── smooth-api-client/   # Library — typed api.smoo.ai bindings + auth wrapper
│   ├── smooth-code/         # Library — ratatui AI coding TUI (an operator client)
│   └── smooth-web/          # Library — embedded Vite SPA via rust-embed
│       └── web/             # React + Vite source (TypeScript)
├── Cargo.toml               # Workspace root (engine is a path-dep to ../smooth-operator)
├── rustfmt.toml             # Format: 160 width, field init shorthand
├── install.sh               # Curl installer
└── .claude/hooks/           # Worktree enforcement
```

> **EPIC th-c89c2a collapsed the microVM substrate onto the operator.** The
> per-VM crates (`smooth-bigsmooth`, `smooth-wonk`, `smooth-narc`,
> `smooth-operative`, `smooth-scribe`, `smooth-archivist`) and the local
> `smooth-operator` engine crate are **gone**: `th daemon` now hosts
> smooth-operator's `LocalServer` directly (engine consumed as a path-dep to the
> `../smooth-operator` checkout), with security re-homed onto a kernel sandbox +
> the `smooth-goalie` egress proxy + the `smooth-policy` Gate-1 rule engine.

### Key Crates

- **smooth-cli** (`crates/smooth-cli/`): clap entry point — the `th` binary. `th daemon …` passes through to the `smooth-daemon` binary.
- **smooth-daemon** (`crates/smooth-daemon/`): the always-on personal-agent daemon. Hosts smooth-operator's `LocalServer` (canonical WS protocol + official widget on `:8787`) made durable by a sqlite `StorageAdapter`; runs the **proactive scheduler** (`schedule.rs`/`scheduler.rs` — fires due tasks into the operator as a loopback WS client) and resolves the LLM gateway. `th daemon` / `th daemon schedule …` / `th daemon permissions …`.
- **smooth-tools** (`crates/smooth-tools/`): the workspace-scoped agent tools (`read_file`/`write_file`/`edit_file`/`grep`/`list_files`/`bash`) the daemon provides per-turn via the operator's `ToolProvider` seam. Path scoping (`path.rs`), the bash circuit-breaker (`guard.rs`), and **Gate-1 deny enforcement** (`permission.rs`, loaded from `~/.smooth/permissions.toml`).
- **smooth-policy** (`crates/smooth-policy/`): shared policy types **and the Gate-1 auto-mode rule engine** (`auto_mode.rs`): `Decision` (deny/ask/allow), `Matcher` (Claude-Code `Tool(pattern)` syntax), `PermissionRules` with deny>ask>allow precedence, bash compound-split, `from_toml`.
- **smooth-goalie** (`crates/smooth-goalie/`): the egress allowlist proxy — the daemon's network boundary (exact-host allowlist, JSON-lines audit). Formerly delegated to the in-VM Wonk; now standalone.
- **smooth-cast** (`crates/smooth-cast/`): coding-harness extensions to the operator engine — the `th code` coding workflow, skill discovery, and harness roles (fixer/oracle/chief/intent_classifier) the published generic engine no longer ships.
- **smooth-pearls** (`crates/smooth-pearls/`): built-in pearl tracker (dependency-graph work items). Dolt-backed via `smooth-dolt` Go binary. Types: `Pearl`, `PearlStore`, `PearlStatus`, `PearlUpdate`, `PearlQuery`, `SmoothDolt`, `Registry`. Also stores session messages + memories.
- **smooth-diver** (`crates/smooth-diver/`): pearl lifecycle manager + project-management service.
- **smooth-plugin** (`crates/smooth-plugin/`): trait-based plugin system for extending Smooth with CLI commands, API routes, TUI views, and operator tools.
- **smooth-api-client** (`crates/smooth-api-client/`): typed `api.smoo.ai` bindings generated from its `openapi.json`, plus an auth wrapper (token store, bearer middleware, refresh-on-401).
- **smooth-code** (`crates/smooth-code/`): ratatui AI coding TUI — an **operator client** (`OperatorClient` speaks the canonical WS protocol to `th daemon`). Streaming chat, tool calls, the HITL approve/deny prompt, file browser, git, sessions, model picker.
- **smooth-web** (`crates/smooth-web/`): rust-embed serves a compiled Vite SPA (the operator's web surface).
- **The engine** is consumed as a **path-dep** to the `../smooth-operator` checkout (crates `smooth-operator` / `-server` / `-svc`), not a local crate — so the daemon embeds the operator's `LocalServer`, tool `ToolProvider` seam, durable `StorageAdapter` seam, and HITL `ConfirmationHook`.

---

## 1a. Using `th` — The Daily-Driver Reference

> **Full doc**: [`docs/Engineering/Using-th-CLI.md`](docs/Engineering/Using-th-CLI.md). The bullets below are the muscle-memory summary; everything below covers what the binary built from this repo can do for you and how to extend it.

`th` is **the** CLI we use across smooth and smooai. Reach for it before `curl`, before the web app, before Supabase Studio. Run `th --help` and `th <command> --help` liberally — every subcommand is self-documenting.

### Auth — `auth.smoo.ai` and what to expect from login

- `th api login` exchanges OAuth2 `grant_type=client_credentials` at `https://auth.smoo.ai/token` and stores a ~60-minute JWT at `~/.smooth/auth/smooai.json`.
- Credential resolution order: `--client-id`/`--client-secret` flags → `SMOOAI_CLIENT_ID`/`SMOOAI_CLIENT_SECRET` env → interactive prompt.
- Mint client credentials in the web app (Org Settings → API Keys) — the secret is shown **once**.
- `th api whoami` shows the active identity (`client:…` for M2M, `user:…` for dashboard), the active org, the JWT TTL, and any `Admin roles` grants (e.g. `super_admin` → cross-org powers).
- `th api orgs list / switch <id>` to change the active org. `th api logout` deletes the cached JWT.
- `th auth login` (no `api`) is **provider** auth — LLM creds at `~/.smooth/providers.json`. Different system. Don't confuse them.

### The high-leverage subtrees

```bash
# Smoo platform — replaces every curl to api.smoo.ai
th api orgs|agents|knowledge|jobs|members|config|keys|observability|profile|testing

# Cross-org admin (planned — pearl th-feebd2, blocked on th-abc4e2)
th admin onboard-customer / mint-key / set-secret / org list|show

# Jira — replaces curl -u "$JIRA_EMAIL:$JIRA_API_TOKEN" .../rest/api/3/...
th jira sync / status

# Pearls (the only spelling — no `th issues` / `th beads` aliases)
th pearls create / ready / list / show / update / close / push / pull

# Worktrees, sandbox/operators, audit, cache, service
th worktree create / list / merge / remove
th up / down / status / run / operators / access / inbox
th audit tail · th doctor · th cache list · th service install
th cast models
```

### What lives where (so you put new code in the right place)

```
Need to call api.smoo.ai?
├── Per-org resource (acts on your active org)
│   └── th api <resource> <verb>  →  crates/smooth-cli/src/api/<resource>.rs
├── Cross-org / requires admin grants
│   └── th admin <verb>           →  crates/smooth-cli/src/admin/   (paired API pearl required)
└── Purely local (no api.smoo.ai roundtrip)
    └── Top-level namespace        →  th pearls, th worktree, th cache, th doctor, …
```

| Lives in `th api` | Lives in `th admin` |
|---|---|
| Acts on **your active org** | Acts **across orgs** or on the platform itself |
| Authenticated as M2M client or regular dashboard user | Authenticated as **admin-grant dashboard user** |
| Backed by `/organizations/{org_id}/…` | Backed by `/admin/…` (paired endpoints don't exist yet) |
| `agents`, `knowledge`, `members`, `config`, `jobs`, `keys`, `observability` | `onboard-customer`, `mint-key`, `set-secret`, `org list/show`, `feature-flag set` |
| **Adding one**: file under `src/api/` + clap subcommand | **Adding one**: API endpoint + CLI subcommand together |

### What does NOT belong in `th`

- One-off scripts → `scripts/` in the relevant repo
- `$EDITOR`-driven interactive flows (`th pearls edit` is discouraged for the same reason)
- TUI-only workflows with no scriptable form → ship the headless surface first
- `exec("curl ...")` wrappers with no value-add (auth refresh, error parsing, pagination, typing) → those go in `~/.smooth/plugins/` as file-based plugin manifests, not in the binary

### Adding a `th` subcommand — the checklist

1. **Search** — `rg "th api <something>" crates/`; someone may have started it
2. **Pearl** — `th pearls create --title="th api X: add Y" --type=feature --priority=2`
3. **Worktree** — `th worktree create th-<id>-…`
4. **Code** — clone the nearest sibling under `crates/smooth-cli/src/api/` (they all follow the same shape), register in `src/api/mod.rs` + parent `Commands` enum
5. **Test exhaustively** — colocated `#[cfg(test)]`, happy + error paths (§8 is non-negotiable)
6. **Doc** — update help text **and** `docs/Engineering/Using-th-CLI.md`
7. **Gate** — `cargo fmt && cargo clippy && cargo test && pnpm install:th`
8. **Land** per §10

### The `th-curl-hint` hook

`.claude/hooks/th-curl-hint.sh` flags Bash commands that should be `th` calls and asks before letting them through:

| Pattern | Suggestion |
|---|---|
| `curl … api.smoo.ai` | `th api …` |
| `curl … auth.smoo.ai/token` | `th api login` |
| `curl … atlassian.net/rest/api` | `th jira sync` (or file a pearl) |
| `echo \| gh secret set … --body -` | `scripts/secret-helpers/gh-secret-set` (SMOODEV-879) |
| `pnpm sst secret list` (raw) | `scripts/secret-helpers/sst-secret-list` (SMOODEV-908) |

Override with ` # th-curl-hint:ack reason=…` if you genuinely need raw curl. **Overriding the same hint twice = file a pearl for the missing wrapper.**

### Continuous improvement

`th` is built from this repo. Every gap is a pearl waiting to happen:

- Daily friction → `th pearls create --type=task --priority=3`
- New API surface in `apps/web` → mirror under `th api <resource>` the same week + changeset
- New admin operation → `th admin <verb>` (blocked on `th-feebd2`; file the sub-pearl now)
- Shell-helper pattern that survives more than two uses → promote to a `th` subcommand or a `~/.smooth/plugins/` plugin

---

## 2. Build, Test, Format, Lint

```bash
cargo build                  # Build all crates
cargo test                   # Run all tests (200+ across 10 crates)
cargo fmt                    # Format (rustfmt.toml: 160 width)
cargo clippy                 # Lint (pedantic + nursery)
cargo build --release -p smooth-cli  # Release binary (~10MB)
pnpm install:th              # Build web bundle + cross-compile sandbox runner + install th
pnpm build:web               # Just rebuild the embedded web SPA
pnpm build:runner            # Just cross-compile the sandbox operative (mirrors to ~/.smooth/runner-bin/)
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
  `smooth-operative` binary at `/opt/smooth/bin`, bind-mounts the
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
bash scripts/build-operative.sh         # produces target/aarch64-unknown-linux-musl/release/smooth-operative
```

Re-run `scripts/build-operative.sh` after changing anything under
`crates/smooth-operative/` or its transitive deps.

The in-process path is kept for backwards compatibility and for the existing
headless E2E tests. New features should target the sandboxed path.

### Security Architecture

The sandbox access control system uses named services running inside each microVM:

- **Big Smooth** — READ-ONLY orchestrator in "The Safehouse" VM
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
Dolt database (via the `smooth-dolt` Go binary). Full version control,
sync via dolt's own `refs/dolt/data` git ref + push/pull to remotes.

```
.smooth/dolt/          # Dolt database (content-addressed)
  └── pearls/          # Dolt "pearls" database
```

Tables: `pearls`, `pearl_dependencies`, `pearl_labels`, `pearl_comments`,
`pearl_history`, `sessions`, `session_messages`, `orchestrator_snapshots`,
`memories`.

> **Beads model — `.smooth/dolt/` is NOT git-tracked.** Pearl
> th-975dfe (2026-06-13) flipped this repo to match how beads stores
> its DB at `.beads/embeddeddolt/`: the on-disk store is gitignored
> and sync happens via dolt's custom `refs/dolt/data` ref pushed
> alongside normal git refs. Reason: noms files are mutable binary
> pointers Dolt rewrites on every open; tracking them in git produced
> recurring merge conflicts when main moved forward while a feature
> worktree was open, even when the worktree never touched dolt. The
> ref-based sync was always available; we just don't materialize the
> files in git anymore.
>
> **Implications:**
> - `git clone` of a fresh checkout has no `.smooth/dolt/` on disk.
>   `th pearls init` detects the missing dir + the `origin` remote
>   and runs `smooth-dolt clone` to bootstrap from `refs/dolt/data`
>   automatically. No manual `th pearls pull` needed for first-time
>   setup.
> - `.gitignore` carries the entry — `th pearls init` adds it
>   idempotently if missing, so existing repos onboard with one
>   command.
> - PR #94 (linked-worktree auto-commit guard) becomes
>   belt-and-suspenders. Same with smooai's
>   `.gitattributes merge=binary` lines on noms files (any repo
>   that still tracks dolt should keep those as a transitional fix).

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
