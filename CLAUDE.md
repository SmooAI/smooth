# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository. It holds the hard rules and core commands; detail lives in the `docs/` vault ([`docs/Home.md`](docs/Home.md)) behind one-line pointers.

**Use Context7 MCP server for up-to-date library documentation.**

> **CRITICAL: All feature work MUST happen in a git worktree.** Never edit source code or commit directly on `main` in `~/dev/smooai/smooth/`. A `PreToolUse` hook enforces this.

## Project Overview

Smooth is the Smoo AI CLI and orchestration platform — a **single Rust binary** (`th`) that coordinates Smooth Operators (AI agents). Zero runtime dependencies.

- Big Smooth **is** `smooth-daemon`. The agent engine is the external `smooth-operator` crate (repo SmooAI/smooth-operator) — **not in this workspace**; don't look for `crates/smooth-operator/`.
- The microVM stack (per-task VMs, `smooth-operative`, `smooth-bigsmooth`/`narc`/`scribe`/`archivist` crates) was removed 2026-07 (pearl th-f4a801, [ADR-004](docs/Decisions/ADR-004-remove-microvm-sandbox-stack.md)).

## 1. Workspace

`ls crates/` is the source of truth. Per-crate detail: [`docs/Architecture/Workspace-Crates.md`](docs/Architecture/Workspace-Crates.md).

| Crate               | Role                                                                                      |
| ------------------- | ----------------------------------------------------------------------------------------- |
| `smooth-cli`        | Binary `th` — clap entry point; platform subcommands in `src/smooai/`, admin `src/admin/` |
| `smooth-daemon`     | Big Smooth — always-on personal-agent daemon hosting smooth-operator's `LocalServer`      |
| `smooth-tools`      | Agent tools (fs/grep/bash/…) + the opt-in kernel OS sandbox (`sandbox.rs`)                |
| `smooth-policy`     | Policy types, TOML parsing, auto-mode, ext trust, settings registry, auth paths           |
| `smooth-goalie`     | HTTP forward proxy = the daemon's egress boundary                                         |
| `smooth-pearls`     | SQLite pearl tracker (`~/.smooth/pearls.db`), memories, agent mail (`~/.smooth/mail.db`)  |
| `smooth-cast`       | Coding-harness bits the published engine dropped (`th code` loop, skills, cast roles)     |
| `smooth-code`       | `th code` ratatui coding TUI                                                              |
| `smooth-diver`      | Pearl lifecycle manager + Jira sync                                                       |
| `smooth-flow`       | SmoothFlow engine: sessions under tmux, PTY streaming, supervision, fan-out               |
| `smooth-tmux`       | tmux driver (drives Claude Code for `th claude`)                                          |
| `smooth-api-client` | Generated api.smoo.ai client + auth wrapper                                               |
| `smooth-web`        | Embedded Vite SPA (`web/`, React + TS) via rust-embed                                     |

Also: `apps/smoothflow/` (macOS SmoothFlow app), `rustfmt.toml` (160 width), `install.sh`.

## 2. Using `th`

`th` is **the** CLI across smooth and smooai — reach for it before `curl`, the web app, or Supabase Studio. Every subcommand has `--help`; appending `ai` to a command path (`smoo org ai`) prints a markdown guide. Interface contract: [`docs/Engineering/CLI-Spec.md`](docs/Engineering/CLI-Spec.md) (read before adding/reshaping a command). Daily-driver reference (auth, every subtree, `smoo api` vs `smoo admin`, the add-a-subcommand checklist, the `th-curl-hint` hook): [`docs/Engineering/th-CLI-Daily-Driver.md`](docs/Engineering/th-CLI-Daily-Driver.md); exhaustive: [`docs/Engineering/Using-th-CLI.md`](docs/Engineering/Using-th-CLI.md).

- Everything that talks to smoo.ai lives under **`smoo <resource> <verb>`** (= `th smoo …`). `smoo auth` is the ONE identity surface; sessions live in `~/.config/smooth/auth/`. Provider (LLM) creds are separate: `~/.smooth/providers.json`.
- `th attest <check>… | --all` — run the repo's `scripts/ci/<name>.sh` and credit passes as `ci-attest/<check>` statuses. Run it **instead of `git push`**.
- `th ci-queue run --lock cargo -- <cmd…>` — run heavy checks through the machine-wide queue (exit 75 = no slot).
- `th harness enable claude-code|codex|opencode|cursor|all` (idempotent; doubles as update) · `th pkg install <source>` · `th settings list|set` (NOT `th config`, which is `smoo config`).
- `th jira sync [--dry-run] [--pull] [--push]` — reconcile-only by default.
- Gaps in `th` are pearls: friction → `th pearls create --type=task --priority=3`; overriding the same curl hint twice → file a pearl for the wrapper.

## 3. Build, Test, Format, Lint

```bash
cargo build / cargo test / cargo fmt / cargo clippy   # clippy: pedantic + nursery
cargo build --release -p smooth-cli  # Release binary
pnpm install:th              # Build web bundle + install th FROM LOCAL SOURCE (the dev loop)
pnpm install:th:brew         # Install the latest RELEASED th via Homebrew
pnpm build:web               # Rebuild the embedded web SPA (dev: cd crates/smooth-web/web && pnpm dev → :3100)
pnpm test:hooks              # Self-check the smooth-agent PreToolUse worktree guard
pnpm format / format:check   # oxfmt
```

- **PreToolUse hooks block on exit 2 and ONLY exit 2.** Any other non-zero exit is a non-blocking error and the tool runs anyway. Keep new deny paths at 2.
- **After any install, `th --version` must match `git log -1`.** `~/.cargo/bin/th` does not automatically win on `PATH`; `install:th` repoints the `/usr/local/bin/th` symlink via `scripts/dev-link-th.sh` (skip with `SMOOTH_NO_DEV_LINK=1`).
- Details (why, footguns): [`docs/Engineering/Dev-Loop-and-Landing.md`](docs/Engineering/Dev-Loop-and-Landing.md).

## 4. Coding Style

- **Rust**: edition 2021, max_width 160, field init shorthand; `unsafe_code = "forbid"`, `unused_must_use = "deny"`; clippy pedantic + nursery; `anyhow` for errors, `thiserror` for library errors; `tracing` for logging.
- **Web**: Vite + React 19 + Tailwind CSS 4; oxfmt + oxlint.
- **Everything not Rust**: `oxfmt` is the only formatter, and its `format` job runs **ungated on every PR** — markdown and changesets are not exempt. Backtick `snake_case` identifiers in docs (oxfmt reads them as emphasis). Exclusions live in `.oxfmtrc.json` `ignorePatterns`.

## 5. Architecture pointers

- **Daemon**: no bespoke server or agent loop — hosts `LocalServer` in-process (`serve_local_flavor` in `operator.rs`); module map in [`docs/Architecture/Daemon-Modules-and-Security.md`](docs/Architecture/Daemon-Modules-and-Security.md).
- **Security layers**, in order: (1) permission gate + `DenyPolicy`, installed FIRST; (2) Narc (`hooks/narc.rs`) detectors + secret redaction; (3) kernel OS sandbox — **opt-in, OFF by default, macOS only** (`th settings set sandbox.enabled true` or `SMOOTH_SANDBOX=1`). Sidekicks get layers 1–2 (`hooks/sidekick.rs`). `SandboxedCommand` is the only way `bash` spawns.
- **Data**: every project's pearls in `~/.smooth/pearls.db` (keyed by main-checkout root, so worktrees share them); agent mail in `~/.smooth/mail.db`. Layout: [`docs/Architecture/Data-Layout.md`](docs/Architecture/Data-Layout.md).
- **SmoothFlow**: [`docs/Architecture/SmoothFlow.md`](docs/Architecture/SmoothFlow.md).

## 6. Pearls + Jira

Pearls (`th pearls`, the only spelling) is the primary tracker; Jira (SMOODEV) is the external source of truth.

```bash
th pearls ready / list --status=open|in_progress / show <id> / blocked
th pearls create --title="Title" --description="..."
th pearls update <id> --status=in_progress   # Claim work
th pearls close <id1> <id2> ...
th pearls checkpoint <id> --note "…" --next "…"   # Handoff checkpoint
th pearls show <id> --handoff                     # Resume a handoff
```

## 7. Git Workflow

All feature work in a worktree: `th worktree create SMOODEV-XX-desc` (`th worktree list` shows them) → `th worktree merge …` → `th worktree remove …`. Never edit source or commit directly on `main`.

## 8. Testing — MANDATORY

> **Every crate, every module, every public function MUST have tests.** No code lands without passing tests.

- Tests colocated (`#[cfg(test)]`); `cargo test`, `cargo clippy` (zero warnings) and `cargo fmt -- --check` must pass before commit.
- Unit (every public fn, error path, edge case), integration (cross-module), property tests where applicable.
- New module: tests first or alongside. Bug fix: regression test that fails without the fix. Security-critical code: **exhaustive** coverage incl. adversarial inputs.

## 9. Changesets

Add a changeset when landing work (`pnpm changeset`). `package.json` is the version source of truth; `scripts/sync-versions.mjs` propagates to `Cargo.toml`/`Cargo.lock`; `release.yml` does Changesets PR → auto-merge → binaries → GitHub Release.

## 10. Landing the Plane

Work is NOT complete until the push succeeds. Full checklist: [`docs/Engineering/Dev-Loop-and-Landing.md`](docs/Engineering/Dev-Loop-and-Landing.md#10-landing-the-plane-session-completion).

1. Quality gates if code changed: `cargo fmt -- --check`, `cargo clippy`, `cargo test`, `cargo build`, `pnpm install:th`.
2. Changeset. 3. Close pearls. 4. Merge to main. 5. Push; `git status` must show up to date. 6. Clean up worktrees/branches.

- NEVER stop before pushing; NEVER say "ready to push when you are" — YOU push.
- All tests, clippy and format checks must pass. If push fails, resolve and retry.
