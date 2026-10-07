# Workspace Crates

#architecture

> [!info] From CLAUDE.md
> What each crate in the `smooth` workspace owns, and what was removed with the microVM stack. Moved verbatim out of `CLAUDE.md` §1 (progressive disclosure); `ls crates/` stays the source of truth.

> **microVM stack removed 2026-07 (pearl th-f4a801).** Big Smooth used to dispatch tasks into per-task microsandbox microVMs (a per-VM cast of Wonk/Goalie/Narc/Scribe). All of it — the VMs, the `smooth-operative` worker binary, the `smooth-bigsmooth` / `smooth-narc` / `smooth-scribe` / `smooth-archivist` crates — is gone. Big Smooth today **is** `smooth-daemon`, and the agent engine is the external `smooth-operator` crate (its own repo, SmooAI/smooth-operator). Git history at the removal PR has the VM path if it ever needs resurrecting; [ADR-004](../Decisions/ADR-004-remove-microvm-sandbox-stack.md) is the record.

## 1. Workspace Structure

Fourteen crates. `ls crates/` is the source of truth; this list is kept in sync with it.

```
smooth/
├── crates/
│   ├── smooth-cli/          # Binary `th` — clap entry point (58 top-level commands)
│   ├── smooth-daemon/       # Binary + lib — Big Smooth: the always-on personal-agent daemon
│   ├── smooth-tools/        # Library — agent tools (fs/grep/bash) + the opt-in kernel OS sandbox
│   ├── smooth-policy/       # Library — policy types, TOML parsing, auto-mode, ext trust
│   ├── smooth-goalie/       # Library + bin — HTTP forward proxy = the egress boundary
│   ├── smooth-pearls/       # Library — SQLite pearl tracker, memories, agent mail
│   ├── smooth-cast/         # Library — coding-harness bits the published engine dropped
│   ├── smooth-code/         # Library — `th code` ratatui coding TUI
│   ├── smooth-diver/        # Library — pearl lifecycle manager + Jira sync
│   ├── smooth-flow/         # Library — SmoothFlow engine: sessions under tmux, PTY streaming, supervision, fan-out
│   ├── smooth-tmux/         # Library — tmux driver (drives Claude Code for `th claude`)
│   ├── smooth-api-client/   # Library — generated api.smoo.ai client + auth wrapper
│   └── smooth-web/          # Library — embedded Vite SPA via rust-embed
│       └── web/             # React + Vite source (TypeScript)
├── apps/
│   └── smoothflow/          # macOS app (Swift/AppKit + libghostty) — the SmoothFlow fleet console; docs/Architecture/SmoothFlow-macOS.md
├── Cargo.toml               # Workspace root
├── rustfmt.toml             # Format: 160 width, field init shorthand
├── install.sh               # Curl installer
└── .claude/hooks/           # Worktree enforcement
```

### Key Crates

- **smooth-cli** (`crates/smooth-cli/`): the `th` binary. clap entry point in `src/main.rs`, 58 top-level commands (60 enum variants: `web-search` is hidden, `admin` is behind the non-default `admin` feature). Platform (api.smoo.ai) subcommands live in `src/smooai/`; cross-org admin in `src/admin/`.
- **smooth-daemon** (`crates/smooth-daemon/`): **Big Smooth.** The always-on, single-tenant personal-agent daemon (EPIC th-c89c2a). It hosts smooth-operator's `LocalServer` in-process — canonical WS protocol, no bespoke agent loop — with durable SQLite storage, scheduled/proactive turns, web push, tailnet exposure, and the security hooks. `th daemon` runs it directly; `th up` also launches it.
- **smooth-operator**: the agent engine (LLM client, agent loop, tool registry + hooks, conversation, checkpointing, cast, permissions, `DenyPolicy`). **It is not in this workspace** — it's a git/crates.io dependency from the separate `SmooAI/smooth-operator` repo. Don't look for `crates/smooth-operator/`.
- **smooth-tools** (`crates/smooth-tools/`): the reusable agent tool surface the daemon registers — `read_file`, `write_file`, `edit_file`, `list_files`, `grep`, `bash`, `cd`, `crawl`, `web_search`, `knowledge_search`, `remember`, `th`, `create_skill`, and (macOS only) `calendar`. Every filesystem path goes through `path::resolve_workspace_path`; `bash` spawns only through `sandbox.rs`'s `SandboxedCommand`, which runs it as the user by default and inside the kernel OS sandbox when `SMOOTH_SANDBOX=1` (§4). `calendar` is the one documented exception (pearl th-94cc4a): it shells `ical` **outside** the sandbox even when it is on, because seatbelt blocks EventKit's XPC/mach lookups — argv-only, fixed binary, verb allowlist (reads + `add`/`update`/`delete`), still Narc-visible. Setup: `th doctor --setup-calendar`. Reads never hand the model raw `ical` JSON: `today`/`upcoming`/`list`/`search` return a compact `{events:[{id,title,start,end,all_day,calendar,location}]}` capped at `limit` (default 25, max 100) with a `more` count + note when cut, `search` defaults to 7 days back / 30 ahead, and only a single-event `show` carries notes and attendees (SMOODEV-3708: the raw output was ~12.5k tokens a call and forced a mid-turn compaction).
- **smooth-policy** (`crates/smooth-policy/`): shared policy types (network, filesystem, pearls, tools, MCP), TOML parsing, glob matching, phase defaults, plus `auto_mode` (permission modes/allow-lists), `ext_trust`, and `smooth_alias`.
- **smooth-goalie** (`crates/smooth-goalie/`): HTTP forward proxy with an exact-host allowlist and JSON-lines audit logging. **Repurposed, not removed** — the microVM-era in-VM/Wonk-delegating mode is dead code paths; what the daemon actually uses is `AuditLogger` + `run_proxy_local` from `start_egress_proxy` (`crates/smooth-daemon/src/lib.rs`), making it the daemon's **egress boundary**. Enabled by `SMOOTH_EGRESS_ALLOWLIST`; `bash` gets `HTTP(S)_PROXY` pointed at it, and only with the opt-in kernel sandbox (`SMOOTH_SANDBOX=1`, macOS) is direct outbound kernel-denied — otherwise the allowlist is advisory.
- **smooth-pearls** (`crates/smooth-pearls/`): built-in pearl tracker (dependency-graph work items). One machine-global SQLite db, `~/.smooth/pearls.db`, rows scoped by canonical project root (pearl th-d3e842). Types: `Pearl`, `PearlStore`, `PearlStatus`, `PearlUpdate`, `PearlQuery`, `MemoryStore`, `Registry`. Agent mail + the agent roster live in a sibling SQLite file, `~/.smooth/mail.db` (`MailStore`, [ADR-010](../Decisions/ADR-010-centralized-agent-mail.md)). No Dolt, no external binary — rusqlite is bundled.
- **smooth-cast** (`crates/smooth-cast/`): the coding-harness specifics the published generic engine dropped — `coding_workflow` (the `th code` outer loop), `skills` discovery, the four harness cast roles (fixer / oracle / chief / intent_classifier), and field-preserving `providers.json` editing.
- **smooth-code** (`crates/smooth-code/`): `th code` — ratatui AI coding TUI: streaming chat, tool calls, file browser, git, sessions, model picker, extensions.
- **smooth-diver** (`crates/smooth-diver/`): Pearl Diver — pearl lifecycle (create on dispatch, close on completion, sub-pearls, deps/labels/costs) plus the bidirectional Jira client.
- **smooth-tmux** (`crates/smooth-tmux/`): dependency-light tmux driver (per-driver socket isolation, bracketed-paste send, full scrollback capture) — how `th claude` supervises Claude Code. Also carries `detect` (the Claude Code pane-state heuristics), shared by `th claude` and SmoothFlow.
- **smooth-flow** (`crates/smooth-flow/`): **SmoothFlow** (epic th-6ac036). Agent/shell sessions run under one long-lived `tmux -L smooth-flow` server (they outlive the daemon), PTY bytes stream to attached clients via a `portable-pty` on `tmux attach`, Claude Code hooks drive state, a supervision tick resumes crashes / schedules usage-limit resumes / guards duplicate resumes, and fan-out races N worktrees. SQLite at `~/.smooth/flow.db`. Hosted by smooth-daemon (`/api/flow/*`); `th flow` is the CLI. See [`docs/Architecture/SmoothFlow.md`](../Architecture/SmoothFlow.md).
- **smooth-api-client** (`crates/smooth-api-client/`): api.smoo.ai client generated at build time by progenitor from `openapi.json`, plus the auth wrapper (token store, bearer middleware, refresh-on-401).
- **smooth-web** (`crates/smooth-web/`): rust-embed serves the compiled Vite SPA. On a WebSocket reconnect the chat (`web/src/operator.ts`) re-binds the conversation on screen (`create_conversation_session` with its `conversationId`) and reloads its history before the composer re-enables — it used to open a fresh conversation and silently send the next message there (SMOODEV-3708). An image-only send goes out with the message `(image attached)`, because the engine rejects an empty one.
- Removed 2026-07 (pearl th-f4a801, in git history): **smooth-bigsmooth** (its role is now smooth-daemon), **smooth-operative** (the per-task worker binary), **smooth-narc** (re-homed as `smooth-daemon/src/hooks/narc.rs`), **smooth-scribe**, **smooth-archivist**, **smooth-wonk**, **smooth-bootstrap-bill**, **smooth-host-stub**, **smooth-credential-helper**.

---

## Related

- [[Home]]
