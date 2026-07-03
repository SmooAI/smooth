# Using the `th` CLI

> **Audience:** humans + Claude Code working in either `smooth/` or `smooai/`.
> **TL;DR:** `th` is the daily-driver CLI for everything Smoo. Reach for it before `curl`, before web-app point-and-click, before opening Supabase Studio. If a workflow doesn't have a `th` subcommand yet, that's a [missing-feature pearl](#extending-th-add-it-when-its-missing), not a reason to fall back to shell scripts.

---

## 1. What `th` actually is

`th` is the single Rust binary built from this repo (`crates/smooth-cli/`). It bundles:

| Layer | Subcommand surface | Backed by |
|---|---|---|
| **Local pearl tracking** | `th pearls …` | Embedded Dolt DB at `<repo>/.smooth/dolt/` |
| **Jira sync** | `th jira sync` | Atlassian REST + Dolt pearl store |
| **Smoo AI platform API** | `th api …` | `https://api.smoo.ai` (auth via JWT at `~/.smooth/auth/smooai.json`) |
| **Provider auth** | `th auth …` | LLM provider credentials at `~/.smooth/providers.json` |
| **Operator orchestration** | `th up`, `th run`, `th operators`, `th access` | Big Smooth daemon; operatives as host subprocesses |
| **Coding TUI** | `th` (no args) or `th code` | smooth-code crate, ratatui |
| **Worktree helpers** | `th worktree create/list/merge/remove` | git plumbing |
| **MCP / plugins / skills** | `th mcp`, `th plugin`, `th skills` | TOML manifests under `~/.smooth/` |
| **Service ops** | `th service`, `th doctor`, `th cache`, `th audit` | local launchd / systemd, `~/.smooth/` |

Run `th --help` and `th <command> --help` liberally — every subcommand is self-documenting.

---

## 2. Auth — how `auth.smoo.ai` works

The Smoo AI platform uses a two-tier identity model that `th` mirrors exactly:

```
┌──────────────────────────┐         ┌────────────────────────┐
│ Dashboard user (B2C OAuth│         │ M2M client             │
│ — Supabase Google login) │         │ (client_id +           │
│                          │         │  client_secret)        │
└────────────┬─────────────┘         └────────────┬───────────┘
             │ planned (th-abc4e2)               │ today
             │                                   │
             ▼                                   ▼
   ┌────────────────────────────────────────────────────────┐
   │           https://auth.smoo.ai/token                    │
   │   OAuth2 token endpoint — accepts both grant types,     │
   │   returns a short-lived JWT (~60min) scoped to an org   │
   └────────────────────────────┬───────────────────────────┘
                                │
                                ▼
              JWT cached at ~/.smooth/auth/smooai.json
                                │
                                ▼
                ┌──────────────────────────────┐
                │   https://api.smoo.ai/…      │  ← all `th api` calls
                └──────────────────────────────┘
```

### Logging in today (M2M client_credentials)

```bash
th auth login --m2m                # interactive — prompts for client_id + secret
SMOOAI_CLIENT_ID=…  SMOOAI_CLIENT_SECRET=… th auth login --m2m   # env-driven (CI, scripts)
th auth login --m2m --client-id=… --client-secret=…              # flag-driven
```

Credential resolution order (first present wins):

1. `--client-id` / `--client-secret` flags
2. `SMOOAI_CLIENT_ID` / `SMOOAI_CLIENT_SECRET` env vars
3. Interactive prompt

The exchange happens against `https://auth.smoo.ai/token` with `grant_type=client_credentials` and `provider=client_credentials`. The response is a JWT (with org claims, role claims, expiration) that `th` stores at `~/.smooth/auth/smooai.json` and replays as `Authorization: Bearer …` on every `th api` call.

### Where client credentials come from

- **Web app**: Organization Settings → API Keys → "Create API key". The secret is shown **once**. Copy immediately or you'll regenerate.
- **`th api keys create`**: same thing from the CLI, **but** it currently requires a dashboard-user token (see [§4 "th admin" gap](#4-the-th-admin-gap-and-the-onboarding-collapse)). Today that means the web app is the practical source.

### Verifying you're logged in

```bash
th auth whoami
# Identity     client:bee846cc-...        ← the M2M client_id (or user:… if dashboard auth)
# Email        brent@smoo.ai
# Admin roles  super_admin (1)            ← present iff your client/user has admin grants
# Org          8be5f5fd-…  Smoo AI        ← the active org for subsequent calls
# Expires      59m left                   ← JWT TTL
# Stored at    /Users/brentrager/.smooth/auth/smooai.json
```

If you see `super_admin` in `Admin roles` you have *cross-org* powers — every `th api` call will succeed against any org you target with `--org <id>`. Treat that token with the same care as a root AWS key.

### Switching orgs

```bash
th org list                               # see what you have access to (alias of `th api orgs list`)
th org switch <id|name>                   # persist active org across every credential store
th org show                               # details of the active org
th api agents list --org <other-org-id>   # one-off override (no switch)
```

`th org` is the top-level alias for `th api orgs` — `list` / `switch` / `show`. `th auth whoami` prints a reminder of these.

#### Cross-org behavior depends on which session you're using

This is the part that trips people up. "Switching" and `--org`/`--org-id` mean different things for the two session types:

| Session | Active org | Cross-org via `--org` / `--org-id`? |
|---|---|---|
| **User JWT** (`th config` default, `th api` user session) | set by `th org switch` | ✅ **Yes** — a master/super-admin is authorized over child orgs. Read/write child config with `--org-id <child>` and no switch. |
| **M2M** (`--m2m`, and the whole `th admin config` surface) | baked into the token | ❌ **No** — the token is org-locked **server-side**. `--org <child>` → `403 Not authorized for this organization`, and `th org switch` is **cosmetic** for it (it changes local state the server ignores). |

Practical consequences:

- **Setting config values on a child org needs no switch** — `th config set KEY VALUE --org-id <child>` works on the user JWT (master admin authorized over children).
- **Creating a config *environment* on an unwired child org** hits the M2M org-lock (`th admin config environments` is M2M / admin-scoped → 403 cross-org). Bootstrap a brand-new child env via the **deploy path** (`prepareSmooConfig` creates the env at deploy) rather than an admin env-create call.
- **To genuinely act *as* another org's M2M identity**, use named profiles — `th auth profile` bundles a user + M2M identity per org; select with `--profile <name>` / `SMOOAI_PROFILE`.
- **Flag spelling**: `--org-id` and `--org` are interchangeable on both `th config` and `th admin config` (each accepts the other as an alias).

### Logout

```bash
th auth logout --m2m                             # deletes ~/.smooth/auth/smooai.json (idempotent)
```

### Provider auth is separate

`th auth login` configures LLM providers (Anthropic, OpenAI, llm.smoo.ai, etc.) at `~/.smooth/providers.json`. It has nothing to do with `auth.smoo.ai`. Different file, different lifecycle, different command tree.

---

## 3. Daily `th api` reference — replace your curls

Everything under `api.smoo.ai` has a typed wrapper. **Stop writing `curl -H "Authorization: Bearer $JWT" https://api.smoo.ai/...`** — it skips auth refresh, doesn't pretty-print, ignores pagination, and goes stale every time the API changes.

### Orgs / membership

```bash
th api orgs list                                   # GET /organizations
th api orgs show                                   # active org details
th api members list --org <id>                     # list seats
th api members invite '{"email":"x@y","role":"admin"}'
th api members invitations
th api members revoke <id> / resend <id>
# Parent/child org relationships (client-portal model). Parent defaults to
# the active org; --type defaults to `manages` (the platform convention).
th admin org link-child <child-org-id>             # link under active org
th admin org link-child <child-org-id> --parent <org> --type manages
th admin org children                              # list active org's children
th admin org unlink-child <child-org-id>           # delete the relationship
```

### Agents (chat agents owned by an org)

```bash
th api agents list                                 # active org
th api agents show <agent-id>
th api agents summary <agent-id>                   # config + status snapshot
th api agents create -                             # raw JSON body on stdin
# mint = the typed front door to create — builds the body for you,
# and for a public chat agent prints the ready-to-paste embed snippet
th api agents mint --name "Support Bot" \
    --summary "Answers product questions" \
    --instructions @prompt.md \
    --allowed-origin https://example.com \
    --color background=#020618 --color primary=#f2a618
th api agents mint --name "Chakra" --brand-from-url https://chakrabpc.com \
    --allowed-origin https://chakrabpc.com   # extract palette → PATCH colors
# --summary defaults to the name if omitted.
# Cross-org (mint into a CHILD org as a parent-org admin): th api sends the
# org-locked M2M, which can't write to a child org. Point the client at your
# user session (acts cross-org) for the write:
#   SMOOAI_AUTH_FILE=~/.config/smooth/auth/profiles/<profile>/smooai-user.json \
#     th api agents mint --name … --org <child-org-id>
th api agents regenerate <agent-id> --generator=<name>
th api agents list-knowledge <agent-id>
th api agents set-knowledge <agent-id> <body>

# SMOODEV-590 — per-agent config is live on all five polyglot servers
# (instructions/personality/greeting/toolConfig/conversationWorkflow).
# update takes either a raw JSON patch body OR typed field flags:
th api agents update <agent-id> --instructions @prompt.md   # instructions.prompt
th api agents update <agent-id> --greeting "Hi, I'm Smoo!"
th api agents update <agent-id> --personality witty         # preset name…
th api agents update <agent-id> --personality '{"preset":"zen","creativity":0.3,"persona":"dry, terse"}'
th api agents update <agent-id> --visibility internal
th api agents update <agent-id> --workflow @workflow.json   # {goal, steps:[{id,intent,criteria,next?}]}
th api agents update <agent-id> --tool-config '{"enabledTools":[{"toolId":"knowledge_search","enabled":true,"authLevel":"none"}]}'
# toolConfig rules: empty enabledTools = FULL tool set; non-empty = restrict
# to enabled=true entries; all-disabled = no tools (fail closed).
# mint accepts the same --personality/--workflow/--tool-config at create time.
# Read any of these back with: th api agents show <agent-id>
```

### Knowledge

```bash
th api knowledge list
th api knowledge show <doc-id>
th api knowledge content <doc-id>                  # raw text
th api knowledge upload '{"title":"…","body":"…"}'
th api knowledge website '{"url":"https://…"}'
th api knowledge process <doc-id>                  # re-run ingestion
th api knowledge update <doc-id> <body>
th api knowledge delete <doc-id>
```

### Config (org-scoped feature flags + values)

For day-to-day get / set / list against `@smooai/config`, the
top-level `th config` command is the muscle-memory shortcut —
auths via the user JWT by default and auto-refreshes via the
stored Supabase refresh_token:

```bash
th config get <key> --environment=<env>             # raw value (use --json to wrap)
th config set <key> <value> --environment=<env>     # parses value as JSON when possible
th config list --environment=<env>                  # key→value map (--json for raw)
th config <sub> --m2m                               # use ~/.smooth/auth/smooai.json instead
th config <sub> --org-id=<id>                       # override active org
```

The full schemas + environments + feature-flag-evaluation surface
still lives under `th api config`:

```bash
th api config schemas
th api config environments
th api config values --environment=production
th api config feature-flag <flag-key>              # evaluate against active org
th api config feature-flag <flag-key> --context=- < ctx.json
```

#### Local config layout + the `schema.json` wire format

Each consumer keeps a `.smooai-config/` directory: `config.ts` (the
`@smooai/config` schema definitions — `publicConfigSchema`,
`secretConfigSchema`, `featureFlagSchema`), `default.ts` (defaults),
`package.json`, and **`schema.json`** — the wire format that
`th config push`/`pull` sync with the org's remote schema.

> **Library vs CLI:** `@smooai/config` (the TypeScript runtime —
> `await secretConfig.get(...)`) is unchanged. Only the operator CLI
> moved from the deprecated `smooai-config` to `th config`.

`schema.json` shape:

```jsonc
{
  "$schema": "...",
  "public":     ["CLOUD_PROVIDER", "REGION", ...],   // UPPER_SNAKE env-var names
  "secret":     ["ANTHROPIC_API_KEY", "CLOUDFLARE_API_TOKEN", ...],
  "featureFlag":["SOME_FLAG", ...],
  "types":      { "cloudProvider": "string", "isLocal": "boolean", ... } // camelCase props
}
```

The tier **arrays** use UPPER_SNAKE env-var names; **`types`** uses the
camelCase config-property names mapped to `"string"`/`"boolean"`. They
are two representations of the same keys, so an unmodified `pull` →
`push` is a clean no-op. To add a secret string key `fooBar`: append
`FOO_BAR` to `secret` **and** `"fooBar": "string"` to `types`.

> **Generating `schema.json`:** there is currently no generator from
> `config.ts` (`withSmooConfig` is only a webpack DefinePlugin). Get a
> `schema.json` via `th config init` (scaffold) or `th config pull`
> (fetch a remote one). A `th config build` generator is tracked in
> pearl `th-4d1d6c`.

> **Picking a schema:** on an org with **more than one** remote schema,
> `th config pull` refuses to guess — pass `--schema-name <name>` (it
> lists the available names). `--schema-name` on `push` selects an
> **existing** schema to update; to **create** a new one, omit the flag
> and set `"$smooaiName": "<name>"` in `schema.json`.

> **Managing environments without `th admin`:** `th config environments
> list|create|update|delete <…> --org-id <org>` works on the public
> user-JWT surface — a parent-org admin can create a child org's
> `production` environment with it (no internal `th admin`).

### Auth clients — M2M + B2M keys (`th api keys`)

Mint and manage an org's API auth clients. Two types, both first-class:

- **M2M** (`--type m2m`, default) — a server/CI secret (`client_id` +
  **secret key**). Used for `client_credentials` grants at
  `auth.smoo.ai` (the same kind `th auth login --m2m` consumes).
- **B2M** (`--type b2m`) — a browser/frontend **publishable key**
  restricted to an allowlist of origins. The key is exposed to the
  page, so the origin pin is the security boundary — at least one
  `--allowed-origin` is required.

These routes require a dashboard **user** session (`th auth login`) and
403 under M2M. A master admin can target a child org with `--org-id`.

```bash
th api keys list                                  # M2M + B2M, with origins (--json for raw)

th api keys create                                # M2M (default) — prints secret key ONCE
th api keys create --type b2m \
  --allowed-origin https://app.example.com \
  --allowed-origin https://example.com            # B2M — prints publishable key ONCE

th api keys update <client_id> \
  --allowed-origin https://new.example.com        # replace a B2M client's origins (B2M only)

th api keys rotate <client_id>                     # mint replacement (same type/origins), revoke old
th api keys revoke <client_id>                     # delete a client

th api keys create --type b2m --allowed-origin … --org-id <child>   # master admin, child org
```

The key value is shown exactly once — store it immediately. `--type`
accepts the short `m2m`/`b2m` or the long `machine-to-machine`/
`browser-to-machine`. `rotate` exists because the API has **no in-place
rotation**: it creates a fresh client (new `client_id` + key) of the
same type/origins, then revokes the old one — so update every consumer.
A raw `--body '<json>'` escape hatch is still accepted.

### LLM gateway keys (`th llm`)

Mint and manage an org's `llm.smoo.ai` keys — the LiteLLM virtual keys
scoped to the org's team/budget. `th llm` is the top-level surface over
`api.smoo.ai/organizations/{org_id}/llm-gateway/*`. It authenticates as
the **user** (Supabase JWT) and is org-admin-gated, so it 401s under an
M2M token — run `th auth login` (user flow) first. A master/super admin
can mint for a child org with `--org-id <child>` (the user JWT acts
cross-org).

```bash
th llm overview                       # masked key + month-to-date spend (--json for raw)
th llm create-key                     # mint the org's persistent key — prints the value ONCE
th llm rotate-key                     # invalidate + reissue the persistent key (prints once)
th llm usage --days 30                # spend by model + by day (JSON timeseries)

# additional named keys (e.g. per service / environment)
th llm keys list                      # masked list (--json for raw)
th llm keys create ci                 # mint a named key — prints the value ONCE
th llm keys rotate ci                 # reissue a named key
th llm keys delete ci                 # revoke (soft-delete; name reusable later)

th llm create-key --org-id <child>    # master admin minting for a child org
```

The minted key value is shown exactly once — store it immediately, then
wire it into the gateway provider with `th model login smooai-gateway`.
`create-key` 409s if the org already has a key (rotate instead). Note
this is the **static-key** model: a persistent virtual key, not the
ephemeral JWT→session exchange originally sketched in pearl `th-f7b20f`
(the backend shipped static keys, so `th llm` wraps that).

### Jobs (async queue)

```bash
th api jobs list
th api jobs show <job-id>
th api jobs create <body>
th api jobs update <job-id> <body>
```

### Integrations (SendGrid email)

```bash
th api integrations sendgrid get
th api integrations sendgrid create --from-email sender@acme.com --inbound-email inbound@acme.com [--from-name "Acme Support"]
th api integrations sendgrid delete
th api integrations sendgrid test --to you@example.com
```

The API key is never passed on argv — `create` reads it from `SENDGRID_API_KEY`
or prompts for it (masked). `test` sends a verification email through the
configured integration.

### Keys (M2M auth clients)

```bash
th api keys list                                   # 403 today unless dashboard-user token
th api keys create '{"name":"…","scopes":[…]}'    # secret returned ONCE
th api keys rotate <id>
th api keys revoke <id>
```

### Observability (source maps + telemetry)

```bash
th api observability sourcemaps-upload <dir> --release=<sha> --environment=production
th api observability sourcemaps-list --release=<sha> --environment=production
```

### Testing (report results + manage runs)

Like `th config`, the testing surface is promoted to a top-level
`th testing` command (the same subcommands also live under
`th api testing`). The muscle-memory entry point is **`runs report`** —
it creates a run and submits a CTRF report in one call, so CI never
hand-rolls the create-run → post-results dance:

```bash
th testing runs report <ctrf.json> --environment=ci --tool=vitest --tags=unit,backend
th testing runs report <junit.xml> --junit --tool=nextest --tags=unit,rust   # converts JUnit → CTRF first
th testing runs report <file> --additional-org-ids=<id1>,<id2>               # also report to other orgs
```

`runs report` defaults `--name` to the file's base name, `--tool` to the
CTRF report's own tool name, and `--build-name` / `--build-url` to the
GitHub Actions env (`$GITHUB_SHA`, the Actions run URL) when present. The
lower-level CRUD is there too:

```bash
th testing runs list|show|create|update|delete|results <id>
th testing deployments|cases|environments <sub>
```

This replaces the old `npx @smooai/testing runs report` + `junit-to-ctrf`
combo — one `th` invocation, authed the same way every other `th` command is.

### Profile / products

```bash
th api profile                                     # currently-logged-in user
th api products list                               # billing plans
```

> **Heuristic:** if you catch yourself typing `curl … api.smoo.ai`, stop and run `th api help` — odds are there's a typed subcommand that handles auth + pagination + error formatting for you. The repo's `th-curl-hint` PreToolUse hook will flag the curl and ask you to use `th api` instead.

---

## 4. The `th admin` gap (and the "onboarding collapse")

Today the M2M token flow is fine for *acting on behalf of an org*. It's wrong for **cross-org admin work** — onboarding a new customer, minting a service-to-service key, setting a GH Actions secret, listing every org in the system. Those should not require you to:

1. Open the web app
2. Create an org manually
3. Open Org Settings → API Keys
4. Create an M2M client
5. Copy the secret
6. Paste it into 1Password
7. Paste it into a GH Actions secret
8. Re-login `th api` with the new client

That's the **7-step ceremony** [pearl `th-feebd2`](https://github.com/smoo-ai/smooth/) calls out, and the planned `th admin` surface collapses it to one command:

```bash
# Planned — th-feebd2 (P1) blocked on th-abc4e2 (admin OAuth login)
th admin onboard-customer --name="Acme" --primary-email="ops@acme.com"
# → creates org via api.smoo.ai/admin/organizations
# → mints a B2M key for the new org
# → writes the secret to GH Actions via `gh secret set` (using the helpers from
#   §13a of the smooai CLAUDE.md)
# → emits a `.smoo-admin.env.ts` sidecar so the per-customer infra file can import it

th admin mint-key --org=<id> --kind=b2m|m2m
th admin set-secret <NAME> <value> --org=<id>          # wraps gh-secret-set helper
th admin org list                                       # cross-org (today: not exposed)
th admin org show <id>
```

This requires the **dashboard-user OAuth flow** (pearl `th-abc4e2`) — a localhost-callback Supabase login that produces a *user* JWT carrying the user's admin grants, not a client-credentials JWT scoped to a single org. Until both pearls land, the workarounds are:

- **Org listing**: log into the web app and pull from the URL bar
- **New-customer onboarding**: the 7-step ceremony above
- **Setting GH Actions secrets**: `scripts/secret-helpers/gh-secret-set` (smooai repo §13a)
- **Listing SST secrets**: `scripts/secret-helpers/sst-secret-list` (smooai repo §13a)

If you hit one of these workarounds and there's no `th admin` for it yet, **file a pearl** (see §6).

---

## 5. The other high-leverage subtrees

### Pearls (work tracking)

See the dedicated [Pearls Workflow Context](../../README.md) — `th pearls create / list / ready / show / update / close`. Dolt-backed per project at `<repo>/.smooth/dolt/`, syncable via `th pearls push / pull`. Always prefer this over `TodoWrite` or ad-hoc markdown.

**Durable by default — no silent data loss (pearl th-4a4559).** Pearls used to be lost to the `refs/dolt/data` divergence: a mutation committed only locally, then a later `th pearls pull` moved `main` to the remote tip and orphaned the un-pushed commits. Two guards close that:

- **Auto-push on mutation.** `th pearls create/update/close/dep/comment/label/…` push to the repo's `refs/dolt/data` right after committing (best-effort, quiet when there's no remote/offline). Pearls are durable the moment they're made — no un-pushed window for a pull or re-clone to drop. Opt out with `SMOOTH_PEARLS_NO_PUSH=1` (e.g. bulk/scripted creates that push once at the end).
- **Fail-safe pull.** `th pearls pull` refuses when local `main` is ahead of the remote (commits not yet pushed), telling you to `th pearls push` first — `--force` (`-f`) pulls anyway. (Even forced, the local-only commits stay in Dolt history and can be recovered by resetting `main` to them.)

**Self-healing store (pearl th-03cdb8).** The on-disk Dolt store can get wedged independently of your work — an interrupted GC/archive wipes `noms/manifest` + `repo_state.json`, or a cross-branch git op leaves conflict markers in the binary manifest. Under the beads model the canonical data lives on the remote's `refs/dolt/data`, so any `th pearls` command now **auto-recovers on open**: it diagnoses the corruption, snapshots the broken store aside as `.smooth/dolt.broken-<ts>`, re-clones from `origin`, and continues — printing what it did to stderr. It resolves the origin from the enclosing git repo when `repo_state.json` is the missing file, and never re-clones out from under a running Big Smooth (`smooth-dolt serve`) — those cases tell you to run `th pearls doctor --force` deliberately. For a manual sweep across every db under the root, `th pearls doctor [--auto-repair] [--force]`.

**Remote sync diagnosis (pearl th-53f6b9).** After the local checks, `th pearls doctor` runs a **remote sync** section. A cheap **tip-level check** runs first (pearl th-c42cc4): local dolt branch head vs the remote-tracking head, and the last-synced `refs/dolt/data` tip (dolt's git-remote-cache `FETCH_HEAD`) vs a bounded `git ls-remote` — all-in-sync answers in ~1s with **no clone**. Anything else falls through to the deep probe: it lists the configured dolt remote (no remote → informational skip), temp-clones the remote's `refs/dolt/data` under a bounded timeout (`SMOOTH_DOLT_SYNC_TIMEOUT_SECS`, 30s default; clone failure → "remote unreachable"), and compares histories — heuristically, over the last 500 commits on each side. Each db is classified as **in-sync**, **local-ahead** (run `th pearls push`), **remote-ahead** (run `th pearls pull`), or **diverged, no common ancestor** — the push/pull deadlock (push refused: diverged; pull refused: data-loss guard). The stray-re-init signature (remote has exactly ONE bare "Initialize data repository" commit) is called out specifically with the fix: `th pearls push --force` overwrites only that bare commit. A divergence against *real* remote commits instead recommends inspecting via `smooth-dolt clone <url> /tmp/check` before any force. Doctor also reports whether the branch upstream is configured (unset upstream makes a bare dolt push fail with `remote '' not found`; plain `th pearls push` auto-repairs it via a `-u` retry). This section is read-only — doctor diagnoses and recommends, it never force-pushes.

**Session priming + memories (pearl th-202885).** `th pearls remember "insight"` records a durable project note; `th pearls memories` lists them; `th pearls forget <id>` drops one. `th pearls prime` prints a compact context block — in-progress + open pearls plus recent memories — for an agent to load at session start (`--json` for machine consumption).

### Agent messaging — `th agent` / `th msg` (pearl th-70aaef)

A harness-agnostic, Dolt-backed mailbox: **any** agent (Claude Code, opencode, pi, a shell loop) in **any** session — same machine or not — registers a name and messages other agents. It's all plain `th` calls layered on the pearl store, so it rides the repo's `refs/dolt/data` git ref. Two sessions sharing one checkout's `.smooth/dolt` see each other instantly; **different clones/machines of the same repo sync automatically** — `send`/`register` push and `watch` pulls each poll (`--no-push`/`--no-pull` for a purely local, offline mailbox).

```bash
th agent register --name <handle>          # idempotent; pushes so other clones see you. identity → $SMOOTH_AGENT, else user@host
th agent list                              # who can I reach (online/last-seen)
th msg send --to <name|all> --body "…"     # direct or broadcast; pushes to the repo remote
th msg inbox [--pull] [--unread] [--mark-read] [--json]   # --pull fetches remote first
th msg reply <id> --body "…"               # threads automatically; pushes
th msg thread <id>                         # whole conversation
th msg watch [--interval 5] [--no-pull]    # blocking poll loop, pulls each poll — the "continuously check" primitive
th inbox                                   # alias for `th msg inbox` (default identity)
```

For agents collaborating across **different clones/machines** of the same repo, that repo needs a git remote with `refs/dolt/data` (`th pearls push` once to seed it). For agents not tied to any repo, the fallback is the global `~/.smooth/dolt` store (single-machine).

`th pearls init` injects an **Agent Messaging** section into the repo's `AGENTS.md` (idempotent, between `<!-- th:agent-messaging:* -->` markers) so any harness that reads `AGENTS.md` learns to register + poll without bespoke wiring. Set `$SMOOTH_HARNESS` so `th agent list` shows what tool each agent is. Read/unread is tracked per message via `read_at`; `to = all` broadcasts share read-state (MVP simplification).

### SEP extensions — `th ext` (SEP Phase 3, pearl th-f288ae)

SEP (the Smooth Extension Protocol) extensions are long-lived subprocesses that speak JSON-RPC over stdio to a Smooth host, contributing tools, hooks, event subscriptions, and UI. `th ext` manages the ones installed on this machine; the engine (`smooai-smooth-operator-core`) discovers and loads them, and the frontend renders their `ui/request`s.

```bash
th ext install <source> [--project] [--trust]  # install from a local dir, npm, or git:
                                               #   th ext install ./path                       (local extension dir)
                                               #   th ext install npm:@scope/pkg[@version]     (npm package)
                                               #   th ext install git:github.com/user/repo@ref (git repo)
                                               #   shows declared capabilities, then prompts to trust
th ext search <query...>                        # find extensions: curated index + live npm `smooth-extension` keyword
th ext update [<name>] [--project] [--trust]    # re-fetch packaged (npm:/git:) extensions from their recorded source
th ext list                                     # installed extensions (global + project) with trust state + source
th ext trust <name> [--project]                # trust an installed extension (records its content hash)
th ext remove <name> [--project]               # delete the extension and its trust record
```

**Install sources (SEP Phase 5).** A local path is copied in. An `npm:` package is vendored under `~/.smooth/extensions/.npm` (an `npm install --prefix` tree so its own deps resolve); a `git:` repo is cloned under `~/.smooth/extensions/.git/<host>/<path>` at the given ref (and `npm install`ed if it has a `package.json`). Either way a `~/.smooth/extensions/<name>` symlink to the vendored dir is what the engine discovers, so packaged and local installs load identically. An extension may ship its manifest as `extension.toml` **or** a `smooth` key in `package.json` (synthesized into `extension.toml` at install). The recorded source lets `th ext update` re-fetch and reconcile — an unchanged manifest keeps its trust; a changed one is re-locked (fail-safe).

**Trust is content-hashed and fail-safe.** An extension only loads when it's recorded `trusted` in `~/.smooth/extensions/trust.toml` **and** its `extension.toml` still hashes to the value trust was granted against — editing (or updating) an extension re-locks it until you `th ext trust` again. A non-interactive install (piped/CI) never trusts silently; use `--trust` to opt in explicitly or run `th ext trust <name>` after. Project-scoped extensions live under `<repo>/.smooth/extensions` and win over a same-named global one.

**Extensions can ship skills.** An extension's `[resources] skills = "<dir>"` directory feeds the one canonical skill catalog (`smooth-cast`) — every SKILL under it becomes a `/skill:<name>` (source `extension`), gated on the same content-hashed trust (an untrusted extension contributes no skills). `smooth-cast` is the only skill parser; `th code`'s `/skill` and `/ext` read from it.

> **In the TUI**, `/ext` lists installed extensions with their trust state and declared capabilities. Live command/UI dispatch into a running host reaches the TUI over the daemon event surface (SEP Phase 6); `th ext`, the trust store, skills unification, and the render-block/host substrate are in place now. The engine `Agent` runs in `smooth-operative` (dispatched server-side) and declares all seven `ui/request` kinds (`smooth_code::sep_host::TUI_UI_CAPABILITIES`) via the `TuiUiProvider` delegate.

### Jira sync

```bash
th jira status                                     # check sync configuration
th jira sync                                       # bidirectional pull+push
th jira sync --pull                                # one-way: Jira → pearls
th jira sync --push                                # one-way: pearls → Jira
```

Use this **instead of** raw `curl -u "$JIRA_EMAIL:$JIRA_API_TOKEN" https://smooai.atlassian.net/...` for read/list. Only fall back to curl when you need a Jira REST verb the wrapper doesn't expose (creating issues, transitioning status — both tracked as missing-feature pearls).

### Operator orchestration

```bash
th up                                              # boot Smooth platform (host daemon)
th down                                            # stop
th status                                          # health
th run <pearl-id>                                  # dispatch a pearl to a Smooth Operator subprocess
th operators list / kill / show
th access pending / approve / deny / policy        # access-control review queue
th inbox                                           # messages requiring attention
```

### Claude session supervision (`th claude`)

Drive a Claude Code TUI inside an isolated tmux session and keep it alive
through the account-wide rate-limit throttle ("Server is temporarily limiting
requests · Rate limited"). When that throttle fires, the supervisor backs off
with **full jitter** and **resends the last message** until it lands — instead
of leaving the turn dead on the screen.

```bash
th claude run                                      # launch + supervise an interactive session (attach to drive it)
th claude run "fix the flaky test" --label fixer   # launch with an initial prompt
th claude run --cwd ../some-worktree               # supervise a session rooted elsewhere
th claude ls                                        # list live supervised sessions (id, mode, label)
th claude ls --json
th claude attach <id>                               # hand your terminal to a session (tmux attach; Ctrl-b d to detach)
th claude mode <id> driving|manual|paused           # who drives: Big Smooth | you | nobody
th claude tui                                        # live control dashboard (toggle mode + attach across sessions)
```

`th claude tui` is the **control dashboard**: a live list of supervised sessions
with each one's pane, plus single-key control — `d`/`m`/`p` flip
driving/manual/paused, `a`/`enter` attach, `r` refresh, `q` quit. It's the
"switch between Big Smooth driving and the session itself" surface. The same
control is scriptable via `th claude mode`:

- `driving` — Big Smooth sends input + rescues rate-limits.
- `manual` — you drive (attach); the supervisor only rescues *your* throttled turn.
- `paused` — the supervisor stands down.

How it decides what to do, per poll of the **visible** pane:

- **`temporarily limiting requests` / `Rate limited`** → back off via the shared
  governor and resend the last message (the one it sent, or — if it's babysitting
  a session it didn't launch — the last user turn scraped from scrollback).
- **real `usage limit` / quota** → stop and hand the session back; backing off
  won't help until reset.
- **`esc to interrupt` (working)** → the model is streaming; do nothing (this
  live signal wins over a stale throttle line still on screen).

The session lives as long as the supervisor runs; `Ctrl-C` stops it cleanly.
The rate-limit governor is **pool-aware**: it's the same primitive the planned
1→N farm (one Big Smooth leading N sessions) and N→1 supervisors share, so a 429
on any session backs off the whole pool rather than thundering the herd. Pearls
th-49de8d (driver) / th-a43375 (attach picker). Requires `tmux` on `PATH`.

> **Subscription/ToS note:** this drives your own Claude Code subscription auth.
> Backoff-and-resume that *honors* the limit is fine; running a large unattended
> fleet to maximize a flat-rate plan is the gray zone — keep concurrency
> tasteful, and use the metered API + smooth-operator for true fleet scale.

### Worktree helpers

```bash
th worktree create SMOODEV-XX-desc                 # creates branch + worktree in canonical location
th worktree list
th worktree merge SMOODEV-XX-desc
th worktree remove SMOODEV-XX-desc
```

Both repos enforce worktree usage via a `PreToolUse` hook. `th worktree create` is the path of least resistance.

### Audit

```bash
th audit tail                                      # recent tool-use audit entries
th audit list                                      # actors with audit logs
th audit path                                      # ~/.smooth/audit/
```

### Doctor / cache / service

```bash
th doctor                                          # system health + auto-fix
th cache list / prune / clear
th service install / start / stop / status         # run smooth as a background daemon
```

### LLM cast

```bash
th cast models                                     # list groups from the configured provider via GET /v1/models
                                                   # (also folds in any configured local provider's live models)
```

### Skills — reusable recipes (Claude-Code parity)

A **skill** is a `SKILL.md` (YAML frontmatter + markdown body) that encodes the
right way to do a task, so the agent follows a proven recipe instead of
re-deriving the workflow every time. Discovery reuses `~/.claude/skills/`
verbatim, so an existing Claude Code skill library works with Smooth unchanged.

```bash
th skills list                                     # every discovered skill (name, source, scope, hosts)
th skills show <name>                              # frontmatter + body, incl. any shadowed sources
```

Discovery order (first match wins on name collision):

1. `<workspace>/.smooth/skills/<name>/SKILL.md` — project (highest precedence)
2. `~/.smooth/skills/<name>/SKILL.md` — user-level Smooth
3. `~/.claude/skills/<name>/SKILL.md` — Claude Code (reused as-is)
4. `~/.opencode/skills/<name>/…` — opencode
5. built-ins shipped in the binary (currently `create-skill`)

**How a skill reaches the model.** At dispatch, the operative discovers every
skill and injects a compact catalog (names + descriptions + triggers, bodies
excluded, budget-capped) into its system prompt. When the model decides a skill
fits, it calls the `skill_use("<name>")` tool, which returns the skill's body
(prefixed with a constraints header derived from `scope` / `allowed_tools` /
`allowed_hosts`) into the conversation as instructions to follow. No separate
execution surface — a skill is a prompt that drives the ordinary bash/file/edit
tools.

**Authoring.** Don't hand-write frontmatter — say "make a skill that …" and the
built-in `create-skill` meta-skill drafts the `SKILL.md`, asks where to save it
(project vs. user), and offers a test run.

Frontmatter fields: `name`, `description` (both required), `triggers` (phrases
that hint when to reach for it), `scope` (`sandbox` default / `host`),
`allowed_hosts`, `allowed_tools`. `allowed_tools` / `allowed_hosts` are
currently surfaced to the model as advisory constraints; hard enforcement lands
with the auto-mode permission model (pearl th-515a13).

### Bring-your-own local models (Ollama, LM Studio, …)

`th providers` adds an OpenAI-compatible inference server to
`~/.smooth/providers.json` and points routing slots at it. Any local
server that speaks `GET /v1/models` + `POST /v1/chat/completions` works
(Ollama, LM Studio, llama.cpp, vLLM, …).

**Ollama in three lines:**

```bash
ollama serve &                                     # starts the API on :11434
ollama pull llama3.3                               # or any model you like
th providers detect --yes                          # finds :11434, adds it, picks a default model
```

`th providers detect` (no `--yes`) just probes the common local ports
(Ollama `11434`, LM Studio `1234`) and reports what answered. Add a
server by hand — or a custom port — with:

```bash
th providers add ollama   --url http://localhost:11434/v1 --model llama3.3
th providers add lmstudio --url http://localhost:1234/v1  --model my-model  --max-tokens 8192
th providers list                                  # [local] tag + per-provider max_tokens
th providers remove lmstudio
```

- `--max-tokens` caps the output-token request for that provider. Small
  local context windows are blown by the default 32768 — set it to fit
  the model. It's plumbed through Big Smooth into the operative on
  dispatch (no cap = the 32768 default).
- Writes are field-preserving: re-running `add` with the same id merges
  (only the flags you pass change), and unknown keys / sibling
  providers' `max_tokens` survive. Adding the first provider to an empty
  file wires every routing slot to it.
- Local models show up live in `th cast models` and in the `th code`
  `/model` picker (press Tab for "show all"). Both tolerate the server
  being down — they just skip it.
- `th providers` is the raw/local surface; `th model login <preset>`
  stays the keyed path for the cloud presets (Anthropic, OpenRouter,
  Smoo AI gateway, …).

### Routing slots & the model catalog

`~/.smooth/providers.json` routes seven slots (`coding`, `reasoning`,
`reviewing`, `judge`, `summarize`, `fast`, `default`) to concrete
`model_name`s. The old gateway `smooth-*` slot aliases (`smooth-coding`,
`smooth-fast-gemini`, …) were **removed at the gateway** (SMOODEV-1793) —
any request for one now 400s. Old config files are migrated in place on
load: every `smooth-*` alias (and the since-removed `groq-llama-*`
concretes) is rewritten to its concrete default and saved back, so
existing installs keep working with no manual edit. The canonical
mapping lives in `smooth_policy::smooth_alias`.

The `th code` `/model` picker sources its catalog — use-cases, tier,
$/M cost, benchmarks — from the gateway's live `GET /v1/model/info`
when a gateway provider is configured, so removed models drop out and
new ones appear without a Smooth release. It falls back to a baked
offline catalog when no gateway is reachable, and folds in local
providers' live models either way (Tab for "show all").

---

## 6. Extending `th` — add it when it's missing

`th` is a **single Rust binary in `crates/smooth-cli/`**. Adding a subcommand is cheap — usually <100 LOC including the integration test. The hard part is deciding where it goes. Use this decision tree:

```
Need to call api.smoo.ai?
├── It's a per-org resource (agents, knowledge, jobs, members, config, …)
│   └── Add under `th api <resource> <verb>`  (crates/smooth-cli/src/api/<resource>.rs)
├── It's cross-org / requires dashboard-user grants
│   └── Add under `th admin <verb>`  (crates/smooth-cli/src/admin/, blocked on th-feebd2)
│       — file a sub-pearl that depends on th-feebd2 so it lands once the surface exists
└── It's purely local (no api.smoo.ai roundtrip)
    └── Goes at the top level under its own namespace
        (th pearls, th worktree, th cache, th doctor, …)
```

### What belongs in `th api` vs `th admin`

| Lives in `th api` | Lives in `th admin` |
|---|---|
| Acts on resources owned by **your active org** | Acts **across orgs** or **on the platform itself** |
| Authenticated as an M2M client or a regular dashboard user | Authenticated as an **admin-grant dashboard user** |
| Backed by `/organizations/{org_id}/…` endpoints | Backed by `/admin/…` endpoints (don't exist yet — paired pearl) |
| `agents`, `knowledge`, `members`, `config`, `jobs`, `keys`, `observability` | `onboard-customer`, `mint-key`, `org list/show`, `set-secret`, `feature-flag set` (planned) |
| **Adding one**: just a new file under `src/api/` + clap subcommand | **Adding one**: requires API-side `/admin/...` endpoint + CLI subcommand together |

### What does NOT belong in `th`

- One-off scripts that run once and get deleted → `scripts/` in the relevant repo
- Anything that requires interactive editing of files Claude can't drive headless (`$EDITOR` flows) — same reason `th pearls edit` is discouraged
- TUI-only workflows that have no scriptable form (push the headless surface first, then wrap a TUI around it)
- Wrappers that just `exec("curl ...")` with no value-add (auth refresh, error parsing, pagination, JSON typing) — those go in `~/.smooth/plugins/` as file-based plugin manifests, not in the binary

### How to actually add a subcommand

1. **Search first**: `rg "th api <something>" crates/` — somebody may have started it
2. **File the pearl**: `th pearls create --title="th api X: add Y" --type=feature --priority=2 --description="…"`
3. **Worktree**: `th worktree create th-<id>-th-api-x-add-y`
4. **Add the clap node**: `crates/smooth-cli/src/api/<resource>.rs` (clone the nearest sibling — they all follow the same shape)
5. **Wire it in**: register the new module under `src/api/mod.rs` and the parent `Commands` enum
6. **Test exhaustively**: `#[cfg(test)] mod tests` colocated, covering happy path + at least one error path. Smooth CLAUDE.md §8 is non-negotiable: "No code lands without passing tests."
7. **Update the help text and this doc** — if it's worth shipping it's worth documenting
8. **Run the full gate**: `cargo fmt && cargo clippy && cargo test && pnpm install:th`
9. **Land** per CLAUDE.md §10 ("Landing the Plane")

---

## 7. The `th-curl-hint` hook — why your curl just got flagged

Both repos ship a `PreToolUse` Bash hook (`.claude/hooks/th-curl-hint.sh`) that pattern-matches the command about to run and blocks it with a hint when it sees:

| Pattern | Hint |
|---|---|
| `curl … api.smoo.ai` | Use `th api …` instead |
| `curl … auth.smoo.ai/token` | Use `th auth login --m2m` instead |
| `curl … atlassian.net/rest/api` | Use `th jira sync` (or file a pearl for the missing verb) |
| `gh secret set … --body -` with stdin echo | Use `scripts/secret-helpers/gh-secret-set` to avoid trailing-newline corruption |
| `pnpm sst secret list` (raw) | Use `scripts/secret-helpers/sst-secret-list` to avoid plaintext leakage |

The hook **does not block** legitimate uses (file a pearl, hit override, or use `--body` directly per the helper README) — it nudges. Override by re-running and confirming when prompted. The full hint policy is in `.claude/hooks/th-curl-hint.sh`.

If you find yourself overriding the hint constantly for a particular pattern, that's the loudest possible signal that we have a missing `th` subcommand. **File the pearl.**

---

## 8. Continuous improvement loop

The `th` binary is built from this repo. Every gap is a `th-*` pearl waiting to happen:

- Daily friction → `th pearls create --type=task --priority=3`
- New API surface lands in `apps/web` → mirror it under `th api <resource>` in the same week (and ship a changeset)
- New admin operation → `th admin <verb>` (after `th-feebd2` lands; until then, file a blocked pearl)
- New shell-helper pattern that survives more than two uses → promote to a `th` subcommand or `~/.smooth/plugins/`

`th gain` (RTK proxy, separate binary) tracks token savings on automated operations — surface the heaviest non-`th` curl/jq pipelines there as candidates for promotion.

---

## 9. Cheat sheet

```bash
# Identity
th auth whoami                                                       # who am I, which org, when does my JWT expire
th api orgs list                                                    # what orgs can I see
th api orgs switch <id>                                             # change active org

# Routine querying (replace your curls)
th api agents list
th api knowledge list
th api jobs list
th api config values --environment=production
th api members list
th api keys list                                                    # (403 today on M2M tokens — uses dashboard auth)

# Pearls
th pearls ready
th pearls create --title="..." --type=task --priority=2
th pearls update <id> --status=in_progress
th pearls close <id1> <id2>

# Worktrees
th worktree create SMOODEV-XX-desc
th worktree list
th worktree merge SMOODEV-XX-desc

# Jira (avoid curling rest/api/3 directly)
th jira sync
th jira status

# Sandbox + operators
th up / th down / th status
th run <pearl-id>
th operators list / kill / show
th access pending / approve / deny

# Health
th doctor
th audit tail
th cache list
```

---

## Related

- [Pearls Workflow](../../README.md) — pearl tracking philosophy
- [Security Architecture](../white-paper-security-architecture.md) — the in-VM services `th` orchestrates
- [Extending Smooth](../extending.md) — MCP servers + file-based plugins
- pearl `th-feebd2` — the `th admin` surface
- pearl `th-abc4e2` — dashboard-user OAuth login
