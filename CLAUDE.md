# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

**Use Context7 MCP server for up-to-date library documentation.**

> **CRITICAL: All feature work MUST happen in a git worktree.** Never edit source code or commit directly on `main` in `~/dev/smooai/smooth/`. A `PreToolUse` hook enforces this.

## Project Overview

Smooth is the Smoo AI CLI and orchestration platform — a **single Rust binary** (`th`) that coordinates Smooth Operators (AI agents). Zero runtime dependencies.

> **microVM stack removed 2026-07 (pearl th-f4a801).** Big Smooth used to dispatch tasks into per-task microsandbox microVMs (a per-VM cast of Wonk/Goalie/Narc/Scribe). All of it — the VMs, the `smooth-operative` worker binary, the `smooth-bigsmooth` / `smooth-narc` / `smooth-scribe` / `smooth-archivist` crates — is gone. Big Smooth today **is** `smooth-daemon`, and the agent engine is the external `smooth-operator` crate (its own repo, SmooAI/smooth-operator). Git history at the removal PR has the VM path if it ever needs resurrecting; [ADR-004](docs/Decisions/ADR-004-remove-microvm-sandbox-stack.md) is the record.

---

## 1. Workspace Structure

Twelve crates. `ls crates/` is the source of truth; this list is kept in sync with it.

```
smooth/
├── crates/
│   ├── smooth-cli/          # Binary `th` — clap entry point (57 top-level commands)
│   ├── smooth-daemon/       # Binary + lib — Big Smooth: the always-on personal-agent daemon
│   ├── smooth-tools/        # Library — agent tools (fs/grep/bash) + the kernel OS sandbox
│   ├── smooth-policy/       # Library — policy types, TOML parsing, auto-mode, ext trust
│   ├── smooth-goalie/       # Library + bin — HTTP forward proxy = the egress boundary
│   ├── smooth-pearls/       # Library — Dolt-backed pearl tracker, memories, agent registry
│   ├── smooth-cast/         # Library — coding-harness bits the published engine dropped
│   ├── smooth-code/         # Library — `th code` ratatui coding TUI
│   ├── smooth-diver/        # Library — pearl lifecycle manager + Jira sync
│   ├── smooth-tmux/         # Library — tmux driver (drives Claude Code for `th claude`)
│   ├── smooth-api-client/   # Library — generated api.smoo.ai client + auth wrapper
│   └── smooth-web/          # Library — embedded Vite SPA via rust-embed
│       └── web/             # React + Vite source (TypeScript)
├── Cargo.toml               # Workspace root
├── rustfmt.toml             # Format: 160 width, field init shorthand
├── install.sh               # Curl installer
└── .claude/hooks/           # Worktree enforcement
```

### Key Crates

- **smooth-cli** (`crates/smooth-cli/`): the `th` binary. clap entry point in `src/main.rs`, 57 top-level commands (59 enum variants: `web-search` is hidden, `admin` is behind the non-default `admin` feature). Platform (api.smoo.ai) subcommands live in `src/smooai/`; cross-org admin in `src/admin/`.
- **smooth-daemon** (`crates/smooth-daemon/`): **Big Smooth.** The always-on, single-tenant personal-agent daemon (EPIC th-c89c2a). It hosts smooth-operator's `LocalServer` in-process — canonical WS protocol, no bespoke agent loop — with durable SQLite storage, scheduled/proactive turns, web push, tailnet exposure, and the security hooks. `th daemon` runs it directly; `th up` also launches it.
- **smooth-operator**: the agent engine (LLM client, agent loop, tool registry + hooks, conversation, checkpointing, cast, permissions, `DenyPolicy`). **It is not in this workspace** — it's a git/crates.io dependency from the separate `SmooAI/smooth-operator` repo. Don't look for `crates/smooth-operator/`.
- **smooth-tools** (`crates/smooth-tools/`): the reusable agent tool surface the daemon registers — `read_file`, `write_file`, `edit_file`, `list_files`, `grep`, `bash`, `cd`, `crawl`, `web_search`, `knowledge_search`, `remember`, `th`, `create_skill`, and (macOS only) `calendar`. Every filesystem path goes through `path::resolve_workspace_path`; `bash` runs only inside `sandbox.rs`'s kernel OS sandbox. `calendar` is the one documented exception (pearl th-94cc4a): it shells `ical` **outside** the sandbox because seatbelt blocks EventKit's XPC/mach lookups — argv-only, fixed binary, verb allowlist (reads + `add`/`update`/`delete`), still Narc-visible. Setup: `th doctor --setup-calendar`.
- **smooth-policy** (`crates/smooth-policy/`): shared policy types (network, filesystem, pearls, tools, MCP), TOML parsing, glob matching, phase defaults, plus `auto_mode` (permission modes/allow-lists), `ext_trust`, and `smooth_alias`.
- **smooth-goalie** (`crates/smooth-goalie/`): HTTP forward proxy with an exact-host allowlist and JSON-lines audit logging. **Repurposed, not removed** — the microVM-era in-VM/Wonk-delegating mode is dead code paths; what the daemon actually uses is `AuditLogger` + `run_proxy_local` from `start_egress_proxy` (`crates/smooth-daemon/src/lib.rs`), making it the daemon's **egress boundary**. Enabled by `SMOOTH_EGRESS_ALLOWLIST`; the sandbox points `HTTP(S)_PROXY` at it and kernel-denies direct outbound.
- **smooth-pearls** (`crates/smooth-pearls/`): built-in pearl tracker (dependency-graph work items). Dolt-backed via the `smooth-dolt` Go binary for version control and git sync. Types: `Pearl`, `PearlStore`, `PearlStatus`, `PearlUpdate`, `PearlQuery`, `SmoothDolt`, `Registry`. Also stores session messages and memories. **Agent mail + the agent roster are NOT in Dolt** — `MailStore` (`mail_store.rs`) keeps them in one machine-level SQLite file, `~/.smooth/mail.db` ([ADR-010](docs/Decisions/ADR-010-centralized-agent-mail.md), pearl th-374f85). The old `Mailbox`/`AgentRegistry` Dolt types remain only so pre-migration per-repo data stays readable.
- **smooth-cast** (`crates/smooth-cast/`): the coding-harness specifics the published generic engine dropped — `coding_workflow` (the `th code` outer loop), `skills` discovery, the four harness cast roles (fixer / oracle / chief / intent_classifier), and field-preserving `providers.json` editing.
- **smooth-code** (`crates/smooth-code/`): `th code` — ratatui AI coding TUI: streaming chat, tool calls, file browser, git, sessions, model picker, extensions.
- **smooth-diver** (`crates/smooth-diver/`): Pearl Diver — pearl lifecycle (create on dispatch, close on completion, sub-pearls, deps/labels/costs) plus the bidirectional Jira client.
- **smooth-tmux** (`crates/smooth-tmux/`): dependency-light tmux driver (per-driver socket isolation, bracketed-paste send, full scrollback capture) — how `th claude` supervises Claude Code.
- **smooth-api-client** (`crates/smooth-api-client/`): api.smoo.ai client generated at build time by progenitor from `openapi.json`, plus the auth wrapper (token store, bearer middleware, refresh-on-401).
- **smooth-web** (`crates/smooth-web/`): rust-embed serves the compiled Vite SPA.
- Removed 2026-07 (pearl th-f4a801, in git history): **smooth-bigsmooth** (its role is now smooth-daemon), **smooth-operative** (the per-task worker binary), **smooth-narc** (re-homed as `smooth-daemon/src/hooks/narc.rs`), **smooth-scribe**, **smooth-archivist**, **smooth-wonk**, **smooth-bootstrap-bill**, **smooth-host-stub**, **smooth-credential-helper**.

---

## 1a. Using `th` — The Daily-Driver Reference

> **Full doc**: [`docs/Engineering/Using-th-CLI.md`](docs/Engineering/Using-th-CLI.md). The bullets below are the muscle-memory summary; everything below covers what the binary built from this repo can do for you and how to extend it.

`th` is **the** CLI we use across smooth and smooai. Reach for it before `curl`, before the web app, before Supabase Studio. Run `th --help` and `th <command> --help` liberally — every subcommand is self-documenting.

### Auth — `auth.smoo.ai` and what to expect from login

> **`th auth` is the ONE Smoo AI identity surface.** `th api login` / `logout` / `whoami` were removed (pearl th-16b0ca) — two spellings for one identity was actively confusing, and only `th auth` understands auth profiles. The `th api <resource>` verbs stay; they aren't auth.

- `th auth login` — the **user** browser flow by default on a TTY (`smoo.ai/cli-login`, Supabase session). `--no-browser` for an email + password prompt; `--m2m` to authenticate a service account via OAuth2 `client_credentials` at `https://auth.smoo.ai/token`.
- M2M credential resolution order: `--client-id`/`--client-secret` flags → `SMOOAI_CLIENT_ID`/`SMOOAI_CLIENT_SECRET` env → interactive prompt. Mint the pair in the web app (Org Settings → API Keys) — the secret is shown **once**.
- `th auth whoami` shows both sessions (user + M2M), the active org, expiry, and which file each came from. `th auth logout [--m2m|--all]` clears them.
- `th auth profile` manages named profiles — each bundles a user + M2M session so one host can hold several identities. Select per-command with `--profile <name>` / `SMOOAI_PROFILE`, or set the default with `th auth profile use <name>`.
- **Sessions live under `~/.config/smooth/auth/`** (XDG), in `profiles/<name>/{smooai-user.json,smooai.json}` for named profiles or directly in `auth/` for the default. `~/.smooth/auth/` is the pre-SMOODEV-1739 legacy tree, kept only as a migration backup — nothing should read it.
- Profile resolution lives in `smooth_policy::auth_paths` and is called by **both** `th` and `smooth-daemon` at startup, so the daemon reads the same credentials as the `th` tool it shells out to regardless of how it was launched (th-16b0ca).
- `th auth login` is **not** LLM-provider auth. Provider creds (`~/.smooth/providers.json`) are a separate system — see `th cast models` / `th model`.

### The high-leverage subtrees

```bash
# Smoo platform — replaces every curl to api.smoo.ai
th api orgs|agents|smooth-operator|knowledge|jobs|members|config|keys|observability|profile|testing

# White-label an org — theme + logos (logo re-hosted from a path OR a remote URL).
# `enable` is the live switch and refuses a theme that fails WCAG AA contrast.
th branding show|from-url|set|enable|disable|preview|clear

# Cross-org admin (planned — pearl th-feebd2, blocked on th-abc4e2)
th admin onboard-customer / mint-key / set-secret / org list|show

# Jira — replaces curl -u "$JIRA_EMAIL:$JIRA_API_TOKEN" .../rest/api/3/...
# sync is reconcile-only by default (close pearls done in Jira, transition
# Jira tickets whose pearls are all closed); creating anything is opt-in:
# --pull (Jira→pearls), --push (pearls→Jira), --dry-run previews the plan.
# Config = env vars: JIRA_URL, JIRA_PROJECT, JIRA_EMAIL, JIRA_API_TOKEN.
th jira sync [--dry-run] [--pull] [--push] / status

# Pearls (the only spelling — no `th issues` / `th beads` aliases)
th pearls create / ready / list / show / update / close / push / pull

# Run a repo's CI checks here (or on a build box) and credit the passes as
# `ci-attest/<check>` commit statuses, so the workflow skips those rows.
# Run it INSTEAD of `git push`. Checks are the repo's own scripts/ci/<name>.sh —
# `th attest` knows nothing about any particular repo's checks. Three outcomes:
# pass → success, fail → failure, COULD-NOT-RUN (exit 97) → nothing posted,
# because a status is a claim about the COMMIT, not about your laptop.
th attest <check>… | --all | --status | --no-push | --remote <host> | --local

# Worktrees, daemon/operatives, audit, service
th worktree create / list / merge / remove
th daemon · th up / down / status
th run / pause / resume / steer / cancel / approve / operatives / access / inbox
th audit tail · th doctor · th service install
th cast models
```

### What lives where (so you put new code in the right place)

```
Need to call api.smoo.ai?
├── Per-org resource (acts on your active org)
│   └── th api <resource> <verb>  →  crates/smooth-cli/src/smooai/<resource>.rs
├── Cross-org / requires admin grants
│   └── th admin <verb>           →  crates/smooth-cli/src/admin/   (paired API pearl required)
└── Purely local (no api.smoo.ai roundtrip)
    └── Top-level namespace        →  th pearls, th worktree, th doctor, …
```

| Lives in `th api` | Lives in `th admin` |
|---|---|
| Acts on **your active org** | Acts **across orgs** or on the platform itself |
| Authenticated as M2M client or regular dashboard user | Authenticated as **admin-grant dashboard user** |
| Backed by `/organizations/{org_id}/…` | Backed by `/admin/…` (paired endpoints don't exist yet) |
| `agents`, `knowledge`, `members`, `config`, `jobs`, `keys`, `observability` | `onboard-customer`, `mint-key`, `set-secret`, `org list/show`, `feature-flag set` |
| **Adding one**: file under `src/smooai/` + clap subcommand | **Adding one**: API endpoint + CLI subcommand together |

### What does NOT belong in `th`

- One-off scripts → `scripts/` in the relevant repo
- `$EDITOR`-driven interactive flows (`th pearls edit` is discouraged for the same reason)
- TUI-only workflows with no scriptable form → ship the headless surface first
- `exec("curl ...")` wrappers with no value-add (auth refresh, error parsing, pagination, typing) → those go in `~/.smooth/plugins/` as file-based plugin manifests, not in the binary

### Adding a `th` subcommand — the checklist

1. **Search** — `rg "th api <something>" crates/`; someone may have started it
2. **Pearl** — `th pearls create --title="th api X: add Y" --type=feature --priority=2`
3. **Worktree** — `th worktree create th-<id>-…`
4. **Code** — clone the nearest sibling under `crates/smooth-cli/src/smooai/` (they all follow the same shape), register in `src/smooai/mod.rs` + parent `Commands` enum
5. **Test exhaustively** — colocated `#[cfg(test)]`, happy + error paths (§8 is non-negotiable)
6. **Doc** — update help text **and** `docs/Engineering/Using-th-CLI.md`
7. **Gate** — `cargo fmt && cargo clippy && cargo test && pnpm install:th`
8. **Land** per §10

### The `th-curl-hint` hook

`.claude/hooks/th-curl-hint.sh` flags Bash commands that should be `th` calls and asks before letting them through:

| Pattern | Suggestion |
|---|---|
| `curl … api.smoo.ai` | `th api …` |
| `curl … auth.smoo.ai/token` | `th auth login` (`--m2m` for a service account) |
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
cargo test                   # Run all tests (2000+ across the 12 crates)
cargo fmt                    # Format (rustfmt.toml: 160 width)
cargo clippy                 # Lint (pedantic + nursery)
cargo build --release -p smooth-cli  # Release binary (~10MB)
pnpm install:th              # Build web bundle + install th FROM LOCAL SOURCE (the dev test loop)
pnpm install:th:brew         # Install the latest RELEASED th via Homebrew (no source build; ignores local changes)
pnpm build:web               # Just rebuild the embedded web SPA
pnpm test:hooks              # Self-check the smooth-agent PreToolUse worktree guard
```

> **PreToolUse hooks block on exit 2 and ONLY exit 2.** Any other non-zero exit
> is a non-blocking hook error and Claude Code runs the tool anyway — which is
> how `enforce-worktree.sh` sat at `exit 1` and never blocked a single edit on
> main. `pnpm test:hooks` pins the exit codes; keep new deny paths at 2.

> **`pnpm install:th` installs to `~/.cargo/bin/th`, which does NOT automatically win on `PATH`.** The menu bar's "Install th CLI…" symlinks `/usr/local/bin/th` (or `~/.local/bin/th`) at `Big Smooth.app/Contents/Resources/bin/th`, and those dirs usually come first — so a successful dev install can silently keep serving the older bundled binary while you debug a stale `th` (pearl th-fd9d98 lost real time to exactly this). `install:th` now ends with `scripts/dev-link-th.sh`, which repoints that symlink at your build; it only ever rewrites a **symlink**, warns and leaves regular files (Homebrew, manual copies) alone, and is skipped by `SMOOTH_NO_DEV_LINK=1`. Check with `bash scripts/dev-link-th.test.sh`.
>
> **Sanity check after any install:** `th --version` prints the commit it was built from — compare it to `git log -1`. If they differ, you are testing the wrong binary.

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

## 4. Key Modules (smooth-daemon)

Big Smooth has **no bespoke server and no bespoke agent loop**. It hosts
smooth-operator's `LocalServer` (canonical WS protocol + widget) and adds its
own routes through the engine's `serve_routes` seam. Entry point:
`serve_local_flavor` in `operator.rs`.

| Module | Purpose |
|---|---|
| `lib.rs` | Crate root; `serve_local_flavor` re-export + `start_egress_proxy` (the goalie egress boundary) |
| `operator.rs` | The local deployment flavor — builds and runs the operator `LocalServer` in-process, wires tool providers and hooks |
| `operator_storage.rs` | Durable SQLite `StorageAdapter` so conversations/sessions survive restart (no Postgres) |
| `hooks/mod.rs` | The two engine `ToolHook`s installed on every per-turn registry: permission gate, then Narc |
| `hooks/narc.rs` | `NarcHook` — regex detectors on tool args (secrets, prompt injection, dangerous shell), LLM-judge escalation, secret redaction in `post_call` |
| `config.rs` | Daemon config + LLM credential resolution (env → providers.json → gateway), egress config |
| `schedule.rs` / `scheduler.rs` | Proactive/scheduled turns; `SqliteScheduleStore` persists them, the tick loop fires them via a `TurnDriver` |
| `search.rs` | `GET /search` — the `@`-mention autocomplete backend for the web composer |
| `cwd_route.rs` | `GET`/`POST /api/session/cwd` — the UI's `/cd` and `/pwd` |
| `auth_login.rs` | Browser OAuth2 + PKCE sign-in to Smoo AI, routed through the daemon (works over a tailnet origin) |
| `push.rs` | Web Push — VAPID-signed notifications to the installed PWA |
| `tailscale.rs` | Best-effort `tailscale serve` exposure of the loopback listener |

### Dispatch

There is no per-task worker process. A message arrives on the operator's
canonical WebSocket, the engine runs the turn in-process, and tools execute
against the host filesystem through `smooth-tools` — `bash` inside the kernel
sandbox, egress through the goalie proxy. Events stream back over the same
canonical WS to every client (`th code`, the web SPA, SDK clients).

> **microVM sandboxed dispatch removed 2026-07 (pearl th-f4a801).** Big Smooth
> used to spawn a per-task microsandbox microVM (mounting a cross-compiled
> `smooth-operative` at `/opt/smooth/bin`, bind-mounting the workspace) with a
> per-VM Wonk/Goalie/Narc/Scribe cast enforcing network + filesystem policy.
> The interim host-subprocess `smooth-operative` dispatch that replaced it is
> also gone. Git history and
> [ADR-004](docs/Decisions/ADR-004-remove-microvm-sandbox-stack.md) have the
> details.

### Security Architecture

Three layers, in the order a tool call meets them:

1. **Permission gate** — the engine's `permission::PermissionHook`, built in
   `smooth-daemon/src/operator::permission_hook`, layered with the daemon's
   embedded declarative `DenyPolicy` circuit-breakers. Installed **FIRST**, so a
   policy deny short-circuits before surveillance and before the tool runs.
   Modes/allow-lists live in `smooth-policy/src/auto_mode.rs`; see
   [`docs/Engineering/Auto-Mode-Permissions.md`](docs/Engineering/Auto-Mode-Permissions.md).
2. **Narc** (`smooth-daemon/src/hooks/narc.rs`) — surveillance. `pre_call` regex
   detectors (secret exfiltration, prompt injection, dangerous shell ops) with
   fail-closed LLM-judge escalation on ambiguous hits; `post_call` redacts
   detected secrets out of the tool result in place.
3. **Kernel OS sandbox** (`smooth-tools/src/sandbox.rs`) — the load-bearing
   layer, because an agent can talk its way past a userspace check but not past
   the kernel. `bash` subprocesses get filesystem **writes** confined to the
   workspace (plus explicit denies on `.git/hooks` and `.git/config`) and
   **reads** denied on credential stores (`~/.ssh`, `~/.aws`, `~/.config/gh`,
   `~/.kube`, `~/.docker`, `~/.gnupg`, `~/.netrc`, and the daemon's own
   `~/.smooth` secrets). With a proxy configured it is also the **egress
   boundary**: direct outbound is kernel-denied except loopback, so traffic must
   pass goalie's exact-host allowlist. `SandboxedCommand` is the only way `bash`
   builds a subprocess — there is no plain-`Command` constructor.

   ⚠️ **macOS only.** Layer 3 is Seatbelt-backed and exists nowhere else
   (th-08e05a). On Linux and Windows `bash` runs unsandboxed with a startup
   warning — layers 1 and 2 still apply, but they are userspace, and the egress
   allowlist drops from a boundary to a suggestion. Before shipping a Windows
   build read [`docs/Architecture/Windows-Security-Posture.md`](docs/Architecture/Windows-Security-Posture.md),
   which enumerates exactly what is exposed there.

Removed with the microVM stack (2026-07, pearl th-f4a801; see git history):
**Wonk** (per-VM access authority), Goalie's per-VM FUSE + iptables enforcement,
and the "Big Smooth is READ-ONLY inside The Safehouse VM" isolation model.

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
- `smooth.db` — Legacy SQLite. No migration command ships any more (`th pearls migrate-from-sqlite` was removed); the file is unread and safe to delete.
- `mail.db` — Agent mail + the agent roster (SQLite; `$SMOOTH_MAIL_DB` overrides). Machine-level on purpose — see [ADR-010](docs/Decisions/ADR-010-centralized-agent-mail.md)
- `agent-sessions/<session_id>` — Handle each harness session registered under (written by the smooth-agent SessionStart hook, rewritten by `th agent claim`/`rename`)
- `audit/` — Rotating tool usage logs per actor
- `providers.json` — LLM credentials
- `auth/` — **legacy** Smoo AI session tree (pre-SMOODEV-1739). Live sessions moved to `~/.config/smooth/auth/` (see §1a); these files remain only as a migration backup.
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
  - **Integration tests**: cross-module interactions (e.g., policy → sandbox, sandbox → goalie egress)
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
