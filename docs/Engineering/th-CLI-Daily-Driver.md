# th CLI Daily Driver

#engineering

> [!info] From CLAUDE.md
> The muscle-memory `th` / `smoo` reference, auth, where new code goes and how to add a subcommand. Moved verbatim out of `CLAUDE.md` §1a; the exhaustive reference is [[Engineering/Using-th-CLI]].

## 1a. Using `th` — The Daily-Driver Reference

> **Full doc**: [`docs/Engineering/Using-th-CLI.md`](../Engineering/Using-th-CLI.md). The bullets below are the muscle-memory summary; everything below covers what the binary built from this repo can do for you and how to extend it.

`th` is **the** CLI we use across smooth and smooai. Reach for it before `curl`, before the web app, before Supabase Studio. Run `th --help` and `th <command> --help` liberally — every subcommand is self-documenting, and appending `ai` to any command path (`smoo org ai`) prints a generated markdown guide. The interface contract lives in [`docs/Engineering/CLI-Spec.md`](../Engineering/CLI-Spec.md) — read it before adding or reshaping a command.

> 📣 **The `smoo` namespace (pearl th-fc32d9).** `th` is two products in one
> binary: the standalone local agent tool (pearls, worktrees, mail, daemon,
> attest, code — no account needed) and the Smoo AI platform CLI. Everything
> that talks to smoo.ai now lives under **`th smoo <resource> <verb>`**, and a
> `smoo → th` symlink (installed by `pnpm install:th` and install.sh) makes
> **`smoo <resource> <verb>`** the customer-facing spelling via argv[0]
> dispatch. The old top-level spellings (`smoo api …`, `smoo auth …`, `smoo config`,
> `smoo crm`, …) still parse as hidden compat aliases, so the snippets below all
> work — but write new docs/skills with the `smoo` spelling. Bare `th agent`
> stays the machine-local mailbox registry; platform agents are `smoo agents`
> (singular aliased inside the namespace).

### Auth — `auth.smoo.ai` and what to expect from login

> **`smoo auth` is the ONE Smoo AI identity surface.** The old `th api login` / `logout` / `whoami` verbs were removed (pearl th-16b0ca) — two spellings for one identity was actively confusing, and only `smoo auth` understands auth profiles. The `smoo api <resource>` verbs stay; they aren't auth.

- `smoo auth login` — the **user** browser flow by default on a TTY (`smoo.ai/cli-login`, Supabase session). `--no-browser` for an email + password prompt; `--m2m` to authenticate a service account via OAuth2 `client_credentials` at `https://auth.smoo.ai/token`.
- M2M credential resolution order: `--client-id`/`--client-secret` flags → `SMOOAI_CLIENT_ID`/`SMOOAI_CLIENT_SECRET` env → interactive prompt. Mint the pair in the web app (Org Settings → API Keys) — the secret is shown **once**.
- `smoo auth whoami` shows both sessions (user + M2M), the active org, expiry, and which file each came from. `smoo auth logout [--m2m|--all]` clears them.
- `smoo auth profile` manages named profiles — each bundles a user + M2M session so one host can hold several identities. Select per-command with `--profile <name>` / `SMOOAI_PROFILE`, or set the default with `smoo auth profile use <name>`.
- **Sessions live under `~/.config/smooth/auth/`** (XDG), in `profiles/<name>/{smooai-user.json,smooai.json}` for named profiles or directly in `auth/` for the default. `~/.smooth/auth/` is the pre-SMOODEV-1739 legacy tree, kept only as a migration backup — nothing should read it.
- Profile resolution lives in `smooth_policy::auth_paths` and is called by **both** `th` and `smooth-daemon` at startup, so the daemon reads the same credentials as the `th` tool it shells out to regardless of how it was launched (th-16b0ca).
- `smoo auth login` is **not** LLM-provider auth. Provider creds (`~/.smooth/providers.json`) are a separate system — see `th cast models` / `th model`.

### The high-leverage subtrees

```bash
# Smoo platform — replaces every curl to api.smoo.ai (smoo == th smoo)
smoo api orgs|agents|smooth-operator|knowledge|jobs|members|config|keys|observability|profile|testing|workflows
smoo auth login|whoami|logout|profile · smoo agents|crm|work|workflows|config|orgs|knowledge|files|testing|branding
smoo analytics|campaigns|drip|audiences|forms|gbp|search-console|sheets|workforce   # MCP-parity batch (th-739bb1…)

# White-label an org — theme + logos (logo re-hosted from a path OR a remote URL).
# `enable` is the live switch and refuses a theme that fails WCAG AA contrast.
smoo branding show|from-url|set|enable|disable|preview|clear

# Cross-org admin (planned — pearl th-feebd2, blocked on th-abc4e2)
smoo admin onboard-customer / mint-key / set-secret / org list|show

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

# Run a heavy check (typecheck, clippy, a test suite) through the machine-wide
# queue: N kernel-flock slots per class, FIFO, held while memory/swap/load/disk
# are under pressure, `nice` priority. Exit 75 = no slot within --max-wait.
# `--lock cargo` = one job per cargo target dir (auto for a bare `cargo …`);
# a job holding a lock never runs at background QoS (priority inversion).
# `th attest`'s local checks already go through it (SMOODEV-3355).
th ci-queue run [--class heavy|light] [--lock NAME] [--qos Q] [--label L] [--timeout S] [--max-wait S] -- <cmd…> / status [--json]

# Coding harnesses. The manifests SmoothFlow launches (built-in claude /
# opencode / codex / th-code + ~/.smooth/harnesses + th pkg packages;
# docs/Engineering/Harness-Manifests.md) — list/show/add, and sort/hide
# what every picker offers (th-0f6126) …
th harness list [--all] [--json] / show <name> / add <path|owner/repo> / hide|unhide <name> / order <name…>
# … and this machine's toolbox setup for Claude Code / Codex / OpenCode /
# Cursor: MCP server, smooth-agent plugin, shared skills, statusline; for
# Codex also the SmoothFlow hooks in ~/.codex/hooks.json + ~/.smooth in the
# sandbox's writable_roots (th-4ad334). enable is idempotent and doubles as
# the update command.
th harness enable claude-code|codex|opencode|cursor|all / status / disable

# SmoothFlow — agent/shell sessions Big Smooth keeps alive under tmux
th flow ls / new / attach / send / approve / kill / snapshot / inbox / handoff
th flow fanout new / pick

# Install an agent package (skills, rules, MCP, hooks) into every harness from
# ONE Claude-plugin-layout source: path, owner/repo[/subdir][#ref], or a
# marketplace.json. Native output per harness + provenance in
# ~/.smooth/pkg/index.toml so rm/status are exact. hooks.json overlays are
# KEY-MERGED (never replace, never translated), rules render as Cursor .mdc
# and as a managed <!-- th-pkg:<name> --> section in Codex/OpenCode AGENTS.md
# (M1, th-6ad314). `th harness enable` is sugar for installing smooth-agent.
# Spec: docs/Engineering/Harness-Packages.md
th pkg install <source> [--harness all|claude-code,codex,opencode,cursor] / list / status [name] / rm <name> / init [dir]

# Machine settings — ~/.smooth/settings.toml over a typed registry of the
# user-facing SMOOTH_* knobs (sandbox.enabled, egress.allowlist, auto_mode,
# model, fast_mode, relay.*, tailscale.serve, cloud_memory). Precedence:
# legacy env var > file > default. `list --json` is the agent view; `set`
# validates and prints the restart command. NOT `th config` (= smoo config).
# Adding a key: smooth_policy::settings::REGISTRY + settings::raw("key").
th settings list [--json] / show <key> / set <key> <value> / unset <key> / explain <key> / path

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
│   └── smoo api <resource> <verb>  →  crates/smooth-cli/src/smooai/<resource>.rs
├── Cross-org / requires admin grants
│   └── smoo admin <verb>           →  crates/smooth-cli/src/admin/   (paired API pearl required)
└── Purely local (no api.smoo.ai roundtrip)
    └── Top-level namespace        →  th pearls, th worktree, th doctor, …
```

| Lives in `smoo api`                                                         | Lives in `smoo admin`                                                             |
| --------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Acts on **your active org**                                                 | Acts **across orgs** or on the platform itself                                    |
| Authenticated as M2M client or regular dashboard user                       | Authenticated as **admin-grant dashboard user**                                   |
| Backed by `/organizations/{org_id}/…`                                       | Backed by `/admin/…` (paired endpoints don't exist yet)                           |
| `agents`, `knowledge`, `members`, `config`, `jobs`, `keys`, `observability` | `onboard-customer`, `mint-key`, `set-secret`, `org list/show`, `feature-flag set` |
| **Adding one**: file under `src/smooai/` + clap subcommand                  | **Adding one**: API endpoint + CLI subcommand together                            |

### What does NOT belong in `th`

- One-off scripts → `scripts/` in the relevant repo
- `$EDITOR`-driven interactive flows (`th pearls edit` is discouraged for the same reason)
- TUI-only workflows with no scriptable form → ship the headless surface first
- `exec("curl ...")` wrappers with no value-add (auth refresh, error parsing, pagination, typing) → those go in `~/.smooth/plugins/` as file-based plugin manifests, not in the binary

### Adding a `th` subcommand — the checklist

1. **Search** — `rg "smoo api <something>" crates/`; someone may have started it
2. **Pearl** — `th pearls create --title="smoo api X: add Y" --type=feature --priority=2`
3. **Worktree** — `th worktree create th-<id>-…`
4. **Code** — clone the nearest sibling under `crates/smooth-cli/src/smooai/` (they all follow the same shape), register in `src/smooai/mod.rs` + parent `Commands` enum
5. **Test exhaustively** — colocated `#[cfg(test)]`, happy + error paths (§8 is non-negotiable)
6. **Doc** — update help text **and** `docs/Engineering/Using-th-CLI.md`
7. **Gate** — `cargo fmt && cargo clippy && cargo test && pnpm install:th`
8. **Land** per §10

### The `th-curl-hint` hook

`.claude/hooks/th-curl-hint.sh` flags Bash commands that should be `th` calls and asks before letting them through:

| Pattern                            | Suggestion                                             |
| ---------------------------------- | ------------------------------------------------------ |
| `curl … api.smoo.ai`               | `smoo api …`                                           |
| `curl … auth.smoo.ai/token`        | `smoo auth login` (`--m2m` for a service account)      |
| `curl … atlassian.net/rest/api`    | `th jira sync` (or file a pearl)                       |
| `echo \| gh secret set … --body -` | `scripts/secret-helpers/gh-secret-set` (SMOODEV-879)   |
| `pnpm sst secret list` (raw)       | `scripts/secret-helpers/sst-secret-list` (SMOODEV-908) |

Override with ` # th-curl-hint:ack reason=…` if you genuinely need raw curl. **Overriding the same hint twice = file a pearl for the missing wrapper.**

### Continuous improvement

`th` is built from this repo. Every gap is a pearl waiting to happen:

- Daily friction → `th pearls create --type=task --priority=3`
- New API surface in `apps/web` → mirror under `smoo api <resource>` the same week + changeset
- New admin operation → `smoo admin <verb>` (blocked on `th-feebd2`; file the sub-pearl now)
- Shell-helper pattern that survives more than two uses → promote to a `th` subcommand or a `~/.smooth/plugins/` plugin

---

## Related

- [[Home]]
