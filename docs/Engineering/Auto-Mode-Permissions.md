# Auto-Mode Permission Engine

> Pearl **th-515a13**. Source: `crates/smooth-bigsmooth/src/auto_mode.rs`.

The auto-mode permission engine is the **primary tool-execution enforcement
layer** now that the microVM stack (Wonk/Goalie, PR #124) is gone. It is a
Claude-Code-style `ToolHook` that gives every tool call a three-way verdict —
**allow / deny / ask** — before the tool runs.

It sits **first** on the operative's `ToolHook` chain (added before Narc in
`build_chat_tools`). Because the engine runs hooks in add-order and a hook that
returns `Err` short-circuits the whole call, an auto-mode `deny` (or a
failed/timed-out `ask`) blocks the tool before Narc or the tool body execute.

## Modes (`SMOOTH_AUTO_MODE`)

| Value | Behaviour |
|---|---|
| `ask` (default; unset/unknown) | Read-only allow, mutating **ask**, dangerous **deny**. |
| `accept-edits` | Like `ask`, but filesystem-**edit tools** (`file_write` / `edit` / `apply_patch` / `create_file`) auto-approve. Bash, network, and unknown tools still ask; hard denies still block. |
| `deny` (aka `dontask` / `headless`) | Like `ask` but **never prompts** — an unmatched `ask` becomes a **deny** (fail-closed CI/headless posture). |
| `bypass` | Allow everything **except** the hard circuit-breakers (`rm -rf /`, dangerous domains, credential paths). |

Spellings are normalized (dashes/underscores/camelCase all accepted).

## The verdict, layered (`decide`, pure + exhaustively tested)

`decide()` is deterministic — no async, no I/O — so it is unit-tested against
adversarial inputs. Precedence is **deny > ask > allow** (a deny always wins).

1. **Credential-path guard** — any command/path referencing `~/.ssh`,
   `~/.aws/credentials`, `id_rsa`, `/etc/shadow`, … is an immediate **deny**
   (read *and* write — this is the lethal-trifecta exfil risk). Survives `bypass`.
2. **Baseline dangerous-CLI / dangerous-domain deny** — reuses
   `smooth_narc::judge::rule_engine_decide` + `DANGEROUS_DOMAIN_SUFFIXES`.
   Survives `bypass`.
3. **Allow-lists** (`WonkGrants`) — user (`~/.smooth/wonk-allow.toml`) + project
   (`<repo>/.smooth/wonk-allow.toml`) + in-memory session grants. Matches on
   host, bash prefix, or tool name.
4. **Compiled-in default posture** — read-only bins (`ls`, `cat`, `grep`,
   read-only `git` subcommands, …) allow; everything mutating asks.

**Compound commands are split** on `&&`, `||`, `;`, `|`, `&`, newlines and each
subcommand must clear on its own, so `ls && rm -rf /` cannot ride in on `ls`.
Wrapper prefixes (`timeout`, `nice`, `env`, …) are stripped before evaluation.

## The `ask` channel

On an `Ask` verdict the async `AutoModeHook` files a `NewAccessRequest` into the
shared `AccessStore` and awaits a human on the same queue the HTTP routes and TUI
drive:

- `GET  /api/access/pending` — snapshot of open requests
- `POST /api/access/approve` / `/api/access/deny` — resolve (with a scope)
- `GET  /api/access/stream` — SSE of pending/resolved/expired events

**Fail-closed:** an unattended ask times out (default 300s) → the call is denied
and the pending entry is expired so the queue doesn't leak.

## Persisting an approval (scopes)

The approver picks a scope; `AutoModeHook::persist_grant` writes it:

| Scope | Effect |
|---|---|
| `Once` | Nothing persisted — the next identical call re-asks. |
| `Session` | Merged into the in-process `SharedWonkGrants` only. |
| `PearlProject` | Appended to `<repo>/.smooth/wonk-allow.toml` **and** merged in. |
| `User` | Appended to `~/.smooth/wonk-allow.toml` **and** merged in. |

Project grants win over user grants on collision (project file loaded last).

## What this is *not*

The permission engine expresses **intent + UX**; it is bypassable by the same
reasoning agent it constrains. The load-bearing boundary in the daemon design is
the **kernel OS-sandbox + egress proxy** layer (see the smooth-daemon epic,
th-c89c2a) — the auto-mode engine is the deterministic Gate-1 in front of it, not
a substitute for kernel enforcement.
