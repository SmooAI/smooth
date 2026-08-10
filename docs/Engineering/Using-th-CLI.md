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

### Refreshing a session (headless)

Every `th api *` call already refreshes an expired token silently in the request path, so you rarely need this. When you want to freshen the stored token **now** — before handing it to a script, or to confirm the session still resolves — use:

```bash
th auth refresh          # refresh the user session
th auth refresh --m2m    # refresh the M2M service-account session
```

It reuses the same silent-refresh path `th api` uses: a **user** session exchanges its Supabase refresh token; an **M2M** session re-mints via `client_credentials` from the stored client_id/secret (no browser, fully headless — M2M has no rotation and never needs a human). It's a no-op (and says so) when the token still has runway. There's no separate `refresh_token` to manage for M2M; the client secret *is* the durable credential.

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

**LLM provider** credentials (Anthropic, OpenAI, llm.smoo.ai, …) live at `~/.smooth/providers.json` and are managed through the model/cast surface (`th model`, `th cast models`) — a different file, lifecycle, and command tree from `auth.smoo.ai`.

`th auth login` is **Smoo AI identity**, not provider auth. (An earlier revision of this doc said the opposite; `th auth` became the single identity surface when `th api login`/`logout`/`whoami` were removed — pearl th-16b0ca.)

### Where sessions are stored

Sessions live under `~/.config/smooth/auth/` (XDG): `profiles/<name>/{smooai-user.json,smooai.json}` for a named profile, or directly in `auth/` for the default one. `~/.smooth/auth/` is the pre-SMOODEV-1739 legacy tree, kept only as a migration backup.

Resolution lives in `smooth_policy::auth_paths` and runs at startup in **both** `th` and `smooth-daemon`, so Big Smooth and the `th` tool it shells out to always read the same credentials — whether the daemon was started by `th up`, launchd, or a bare `nohup smooth-daemon` (pearl th-16b0ca).

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
# Teams (RBAC groupings that hold roles — SMOODEV-2645). Resolve teams by name
# or id, members by email, roles by name. set-members/set-roles are replace-all.
th api teams list                                  # teams + member/role counts
th api teams create "Sales" --description "..."
th api teams rename "Sales" "Revenue"
th api teams set-members "Revenue" jane@acme.com bob@acme.com
th api teams set-roles "Revenue" admin "Sales Rep"
th api teams delete "Revenue"
# CRM reminders (SMOODEV-2646). Target any CRM entity as TYPE:REF (contact,
# company, deal by name/email/uuid; task/proposal/funnel/custom_object by uuid).
# --at parses tomorrow / "next week" / "in 3 days" / 2h / 2026-08-01 / RFC3339.
th api crm remind contact:jane@acme.com --at tomorrow --note "follow up"
th api crm reminders list --mine                   # your pending, soonest first
th api crm reminders list --entity deal:"Acme renewal"
th api crm remind cancel <reminder-id>             # soft-cancel (also `reminders cancel`)
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
th api agents update <agent-id> --extension '{"enabledExtensions":[{"extensionId":"plan-mode","enabled":true,"config":{}}]}'
# SMOODEV-2259 — extensionConfig gates SEP extensions per agent. extensionId is
# kebab-case (SEP extension name); empty enabledExtensions = no extensions (fail closed).
# mint accepts the same --personality/--workflow/--tool-config/--extension at create time.
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

### Smooth Operator (org's always-on dashboard agent)

Drive the org's [Smooth Operator](../Product/Features/Org-Copilot.md) from the CLI —
the same agent behind the dashboard's ⌘J panel. It acts on the org's own data:
knowledge search, CRM lookup/create, analytics questions, template generation,
and drafting + (on confirm) sending email. User-authed (`th auth login`);
401s under M2M.

```bash
th api smooth-operator chat "Find contacts named Jane and draft a follow-up"   # run a turn
th api smooth-operator chat "Make it warmer" --conversation <id>               # continue it
th api smooth-operator chat "..." --json                                       # raw SmoothOperatorTurnResult
th api smooth-operator history <conversation-id>                               # message history
```

> **Transport (SMOODEV-2673).** The buffered REST `chat`/`confirm` routes were
> **deleted**. `chat` now mints a short-lived socket token
> (`POST /organizations/{org}/smooth-operator/token`) and runs one turn over the
> **SEP WebSocket** (`wss://smooth-operator.smoo.ai/ws`) —
> `create_conversation_session` (resume by id) → `send_message` → await
> `eventual_response`. Implementation: `crates/smooth-cli/src/smooai/smooth_operator_ws.rs`
> (hand-rolled: the `smooth-operator` crates are server-side and ship no Rust client).

Destructive tools (e.g. `email.send`) **never auto-run**. The socket *parks the
turn mid-flight* (`write_confirmation_required`) and takes the decision
**inline**, so approval is a flag on `chat` rather than a second command:
without `--confirm` the action is declined and reported ("I did NOT do this
without your approval"), and the turn still completes. The old
`th api smooth-operator confirm` subcommand is **retired** — it now prints an
explanation pointing at `--confirm`.

```bash
th api smooth-operator chat "Send jane@acme.com the follow-up"             # drafts; declines the send, tells you
th api smooth-operator chat "Send jane@acme.com the follow-up" --confirm   # allows the send this turn
```

Responses are buffered JSON (token streaming is phase 2). Every tool run is
audit-logged against the logged-in user.

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

### White-label branding (`th branding`, alias `th brand`)

Top-level, like `th crm` — SMOODEV-2820. Wraps the org's white-label row
(`/organizations/{org}/branding`) and re-hosts logos on our CDN so a partner's
own server is never left as the source of truth for their mark.

```bash
th branding show [--json]                          # LIVE vs staged, swatch table, preview URL
th branding from-url https://partner.example       # DRY RUN — theme, logos, contrast verdict
th branding from-url https://partner.example --apply    # stage it (enabled stays false)
th branding from-url https://partner.example --enable   # stage AND go live
th branding from-url https://partner.example --apply --logo ./real-logo.svg   # override a bad pick
th branding set --app-name "Acme CRM" --primary '#7c3aed' --primary-foreground '#ffffff'
th branding set --logo ./logo.png --logo-dark https://partner.example/dark.svg --favicon ./icon.svg
th branding enable                                 # the live switch
th branding disable                                # keeps the config, stops applying it
th branding preview                                # the ?brandPreview=1 URL
th branding clear --yes                            # delete the row
```

Things worth knowing before you use it:

- **A row existing is NOT enablement.** `--apply` stages; `enable` goes live.
  Staged branding renders at `…/apps?brandPreview=1` for anyone with the link.
- **`enable` refuses a theme that fails WCAG AA (4.5:1).** Shipping an
  unreadable dashboard to a partner is the failure this gate exists for.
  `--force` overrides it, deliberately loudly.
- **`--logo` / `--logo-dark` / `--favicon` take a local path or a remote URL.**
  A remote URL is fetched (http(s) only, no private/loopback/link-local hosts,
  no redirect following, 5 MB cap) and re-uploaded to the org's brand assets.
  `.ico` is rejected — the platform's allowlist is png / jpeg / gif / webp / svg.
- **`set` is partial**, including `themeJson`: the server's PUT replaces that
  whole column, so `th branding` reads-modifies-writes it for you. An empty
  string (`--accent ''`) clears a token; an untouched token stays untouched.
  There is no PATCH on this route, so PUT-with-merge is the permanent contract.
  The merge sends **only keys that carry a value** — it never null-pads absent
  tokens, and it drops nulls already in the row, so one `th branding set` heals
  a row the dashboard poisoned (SMOODEV-2822).
- **`from-url` shows which logo candidate it picked** (`→` vs `○`). The
  extractor can return several per kind and the first isn't always the mark —
  one real run returned the wordmark *and* the page's `og:image` screenshot,
  both as `logo`. Override with `--logo` / `--logo-dark` / `--favicon`.
- **The Aurora meaning tokens are never white-labeled** — `--color-heat-0..5`,
  `--color-ai`, `--gradient-aurora`, ok/warn/crit encode meaning, not chrome,
  and there are deliberately no flags for them.
- **Auth:** the user JWT (`th auth login`), because the M2M session is
  org-locked and 403s on any org whose client you don't hold.
- **Known server gap:** the platform's write validator is still Phase 1, so the
  surface tokens (`--background`, `--card`, `--sidebar`, …) 400 today; the CLI
  turns that into a diagnosis naming the two stale schemas. `from-url` 404s
  until the propose endpoint deploys. Accent tokens, logos, and the
  enable/disable/clear lifecycle all work now.

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

**Write-locked store — leaked `smooth-dolt` processes (pearl th-118847).** A store can be perfectly healthy on disk and still refuse every write: a leaked one-shot `smooth-dolt sql …` process holds it open, so `th pearls create` / `th msg send` die with `Error 1105: cannot update manifest: database is read only` while reads keep working. `th pearls doctor` therefore (a) lists the `smooth-dolt` processes holding **this project's** store — argv[0] must be the `smooth-dolt` binary AND an argv path must sit under this store, so another project's server or an unrelated process is never touched — and (b) probes **write-ability**, not just readability, reporting `✗ store is write-locked by N leaked smooth-dolt process(es)` instead of a misleading `✓ healthy`. The remedy is `th pearls doctor --reap` (implied by `--auto-repair`): SIGTERM → brief grace → SIGKILL for each leaked process, then a re-probe to confirm writes work again. One-shots are only reaped once they're older than `--reap-age-secs` (default 30s — a healthy one lives milliseconds, so the bound only protects a concurrently-running query); a live `smooth-dolt serve` is only reaped with `--force`. **The re-clone is reachable only when the manifest does not read cleanly** — a healthy store can never be re-cloned away from a broken remote. Root cause of the leak: local one-shots now carry a 120s wallclock bound (`SMOOTH_DOLT_QUERY_TIMEOUT_SECS`, `0` disables), so a wedged child is killed and reaped by its parent instead of hanging `th` until the user Ctrl-Cs and orphans it.

**Remote sync diagnosis (pearl th-53f6b9).** After the local checks, `th pearls doctor` runs a **remote sync** section. A cheap **tip-level check** runs first (pearl th-c42cc4): local dolt branch head vs the remote-tracking head, and the last-synced `refs/dolt/data` tip (dolt's git-remote-cache `FETCH_HEAD`) vs a bounded `git ls-remote` — all-in-sync answers in ~1s with **no clone**. Anything else falls through to the deep probe: it lists the configured dolt remote (no remote → informational skip), temp-clones the remote's `refs/dolt/data` under a bounded timeout (`SMOOTH_DOLT_SYNC_TIMEOUT_SECS`, 30s default; clone failure → "remote unreachable"), and compares histories — heuristically, over the last 500 commits on each side. Each db is classified as **in-sync**, **local-ahead** (run `th pearls push`), **remote-ahead** (run `th pearls pull`), or **diverged, no common ancestor** — the push/pull deadlock (push refused: diverged; pull refused: data-loss guard). The stray-re-init signature (remote has exactly ONE bare "Initialize data repository" commit) is called out specifically with the fix: `th pearls push --force` overwrites only that bare commit. A divergence against *real* remote commits instead recommends inspecting via `smooth-dolt clone <url> /tmp/check` before any force. Doctor also reports whether the branch upstream is configured (unset upstream makes a bare dolt push fail with `remote '' not found`; plain `th pearls push` auto-repairs it via a `-u` retry). This section is read-only — doctor diagnoses and recommends, it never force-pushes.

**Session priming + memories (pearl th-202885).** `th pearls remember "insight"` records a durable project note; `th pearls memories` lists them; `th pearls forget <id>` drops one. `th pearls prime` prints a compact context block — in-progress + open pearls plus recent memories — for an agent to load at session start (`--json` for machine consumption).

**Scheduling — pearls that speak up when due (pearl th-01aa6a).** `th pearls schedule <id> <when>` sets an optional `scheduled_at` on a pearl; omit `<when>` to clear it. `<when>` is relative (`+2h`, `30m`, `2d`, `1w`, `tomorrow`, `now`) or absolute (`2026-07-10`, `2026-07-10 09:00`, RFC3339; parsed as UTC). `th pearls due` lists pearls whose time has arrived (`scheduled_at <= now`, not closed, soonest-first). The **prime hook** surfaces a `⏰ Scheduled & due` section above `Ready to work`, so a scheduled pearl automatically "speaks up" at the next session start / compaction once it comes due. `th pearls show` and the `ready`/`list`/`due` lines render a `⏰` marker for scheduled pearls.

### Agent messaging — `th agent` / `th msg` (pearls th-70aaef, th-374f85)

A harness-agnostic mailbox: **any** agent (Claude Code, opencode, pi, a shell
loop) in **any** session on this host registers a name and messages the others.
Since th-374f85 it lives in **one SQLite file per machine** — `~/.smooth/mail.db`
(`$SMOOTH_MAIL_DB` overrides) — not the per-repo Dolt pearl store. That is what
makes it reliable: the mailbox no longer depends on which worktree you happen to
be standing in, a send is an instant local write instead of a ~0.7s Dolt boot
plus a git push, and concurrent agents queue on SQLite's lock instead of wedging
a shared store with `Error 1105: database is read only`. The trade is that mail
no longer crosses machines — see [[../Decisions/ADR-010-centralized-agent-mail]].

```bash
th agent register --name <handle> [--pid $PPID]  # idempotent; resuming a handle keeps its mail
th agent whoami [--json]                   # what handle am I, which store, how much unread
th agent claim <handle>                    # take a durable name; carries your mail over
th agent list                              # who can I reach (presence, branch, current task)
th agent status --status working --task "…"   # publish presence: idle|working|waiting|offline
th msg send <name|all> "…" [--type request] [--priority 2] [--re <id>]
th msg inbox [--unread] [--mark-read] [--limit N] [--json]
th msg ack <id>… | th msg ack --all        # per-recipient read state (alias: `th msg read`)
th msg reply <id> --body "…"               # threads automatically
th msg thread <id>                         # whole conversation
th msg watch [--interval 5] [--once] [--json]  # blocking poll; --once exits on first mail
th inbox                                   # alias for `th msg inbox` (default identity)
```

`th agent`/`th msg` also answer to `th agents`/`th msgs`.

**Identity** resolves `$SMOOTH_AGENT_HANDLE` → `$SMOOTH_AGENT` → the handle the
SessionStart hook recorded for `$CLAUDE_SESSION_ID` (in
`~/.smooth/agent-sessions/`) → `user@short-hostname`. Set `$SMOOTH_HARNESS` so
`th agent list` shows what tool each agent is. `th agent claim` and `th agent
rename` both rewrite the recorded session handle, so the hook and the store stay
in agreement.

**Message types** (`note|request|result|handoff|cancel`) let a recipient triage
without reading; `--priority N` sorts higher first in the inbox. **Read state is
per-recipient**, so acking a `to = all` broadcast consumes only your copy —
the old first-reader-wins behaviour is gone.

**Presence** is `idle|working|waiting|offline` plus a free-form `--task`, and
`th agent list` reaps first: an agent whose recorded pid is dead flips to
`offline`. The pid has to be supplied (`--pid $PPID` from a long-lived
supervisor, or `$SMOOTH_AGENT_PID`) — `th` never records its own, since it exits
immediately and would mark every agent offline a second after registering.

**Dead flags.** `--no-push`, `--pull` and `--no-pull` still parse (the
SessionStart hook and old scripts pass them) but do nothing and print a
deprecation note: there is no remote to sync with. Old per-repo Dolt mailboxes
are not migrated.

`th pearls init` injects an **Agent Messaging** section into the repo's
`AGENTS.md` (idempotent, between `<!-- th:agent-messaging:* -->` markers) so any
harness that reads `AGENTS.md` learns to register + poll without bespoke wiring.

### `th` as an MCP server — `th mcp serve` (epic th-63e572)

`th mcp` has two halves. The `add`/`list`/`remove`/`defaults`/`install` subcommands are a **client** manager — they register *other* MCP servers (Playwright, GitHub, …) for the operator to consume, writing `~/.smooth/mcp.toml`. `th mcp serve` is the **inverse**: it runs `th` *itself* as a stdio MCP **server**, exposing th's surfaces as MCP tools so Claude Desktop / Cursor / Windsurf / VS Code can drive them.

```jsonc
// Claude Desktop / Cursor / Windsurf: ~/.cursor/mcp.json etc.
{ "mcpServers": { "smooth": { "command": "th", "args": ["mcp", "serve"] } } }
// VS Code (Copilot) uses "servers" (not "mcpServers") — otherwise identical.
```

`th mcp serve` speaks JSON-RPC on stdout (built on the `rmcp` SDK) — **do not mix other output onto stdout**; the tools log only to stderr. It exposes two tiers:

- **Local — free, no sign-in.** `pearls_ready` / `pearls_create` act on the pearl store of the workspace the host launched the server in; `remember` / `recall` keep local notes.
- **Your business — behind Sign in with Smoo (`th auth login`).** `ask_business` is the star: one turn of **Smooth Operator**, the org agent, over the SEP WebSocket (the same transport the `th api smooth-operator` CLI now drives) — ask about revenue/CRM/knowledge and draft, or with **explicit approval**, send email. It resolves your active org automatically, and never sends or takes a destructive action without approval: when it pauses on one, it returns the pending action + a `conversation_id`; approve by calling `ask_business` again with `approve=true` and that id. `knowledge_search` is a fast read of the org knowledge base. Both gate on the user session (they 401 under M2M), so unauthenticated calls return a clear "run `th auth login`" message rather than failing opaquely.

The `.mcpb` **Desktop Extension** for one-click install lives in `packaging/mcpb/` (`build-mcpb.sh` stages the `th` binary + manifest and runs `npx @anthropic-ai/mcpb pack`). The same tool layer is what a hosted Streamable-HTTP server at `mcp.smoo.ai` will reuse for the zero-install Claude Desktop connector (pearl th-794b1e).

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
th ext reload <name> [--project] [--trust]     # re-check manifest + trust, then HOT-RELOAD the running daemon's host
th ext remove <name> [--project]               # delete the extension and its trust record
```

**Install sources (SEP Phase 5).** A local path is copied in. An `npm:` package is vendored under `~/.smooth/extensions/.npm` (an `npm install --prefix` tree so its own deps resolve); a `git:` repo is cloned under `~/.smooth/extensions/.git/<host>/<path>` at the given ref (and `npm install`ed if it has a `package.json`). Either way a `~/.smooth/extensions/<name>` symlink to the vendored dir is what the engine discovers, so packaged and local installs load identically. An extension may ship its manifest as `extension.toml` **or** a `smooth` key in `package.json` (synthesized into `extension.toml` at install). The recorded source lets `th ext update` re-fetch and reconcile — an unchanged manifest keeps its trust; a changed one is re-locked (fail-safe).

**Trust is content-hashed and fail-safe.** An extension only loads when it's recorded `trusted` in `~/.smooth/extensions/trust.toml` **and** its `extension.toml` still hashes to the value trust was granted against — editing (or updating) an extension re-locks it until you `th ext trust` again. A non-interactive install (piped/CI) never trusts silently; use `--trust` to opt in explicitly or run `th ext trust <name>` after. Project-scoped extensions live under `<repo>/.smooth/extensions` and win over a same-named global one.

**Extensions can ship skills.** An extension's `[resources] skills = "<dir>"` directory feeds the one canonical skill catalog (`smooth-cast`) — every SKILL under it becomes a `/skill:<name>` (source `extension`), gated on the same content-hashed trust (an untrusted extension contributes no skills). `smooth-cast` is the only skill parser; `th code`'s `/skill` and `/ext` read from it.

> **In the TUI**, `/ext` lists installed extensions with their trust state and declared capabilities. Live command/UI dispatch into a running host reaches the TUI over the daemon event surface (SEP Phase 6); `th ext`, the trust store, skills unification, and the render-block/host substrate are in place now. The engine `Agent` runs in `smooth-operative` (dispatched server-side) and declares all seven `ui/request` kinds (`smooth_code::sep_host::TUI_UI_CAPABILITIES`) via the `TuiUiProvider` delegate.

**Big Smooth's own chat hosts extensions too (pearl th-6d8606).** The daemon loads pre-trusted extensions once at startup into a shared host; chatting with Big Smooth (smooth-web or `/api/chat`) exposes their tools (gated by AutoMode + Narc like every chat tool), their `ui/*` renders via the same `UiRelay` components, and `/cmd args` in the chat box runs a registered extension command directly. `th ext reload <name>` hot-reloads the daemon's live host over `POST /api/ext/reload` (set `SMOOTH_BIGSMOOTH_URL` if not `http://127.0.0.1:4400`); `GET /api/ext` lists what's live. Newly installed extensions need a daemon restart — discovery runs at startup.

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

Drive a Claude Code TUI inside an isolated tmux session, hand it a prompt, and
keep the session alive and inspectable for as long as the supervisor runs.

> **No 429 auto-retry.** The supervisor used to detect the transient throttle
> ("Server is temporarily limiting requests · Rate limited"), back off with
> jitter, and resend the last message. Claude Code now retries that throttle
> internally, so the rescue was removed (pearl th-2d5c45) — it was dead weight
> that could double-send a prompt on top of a model already recovering. The
> **real usage/quota limit is still detected** and still stops the supervisor.

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

- `driving` — Big Smooth sends the session's input.
- `manual` — you drive (attach); the supervisor sends nothing.
- `paused` — the supervisor stands down and only watches.

How it decides what to do, per poll of the **visible** pane:

- **real `usage limit` / quota** → stop and hand the session back; waiting
  won't help until reset.
- **everything else** (working, idle, awaiting approval, error) → keep
  watching. The transient throttle falls in here: Claude Code retries it
  itself, so the supervisor stays out of the way.

The session lives as long as the supervisor runs; `Ctrl-C` stops it cleanly.
Pearls th-49de8d (driver) / th-a43375 (attach picker) / th-2d5c45 (429 rescue
removal). Requires `tmux` on `PATH`.

> **Subscription/ToS note:** this drives your own Claude Code subscription auth.
> Supervising a session you're present for is fine; running a large unattended
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
th doctor                                          # system health + macOS access grants + auto-fix
th doctor --onboard                                # guided first run: walk every not-ready setup step
th doctor --fix-fda                                # macOS: guide the Full Disk Access grant
th doctor --setup-calendar                         # macOS: install `ical` + drive the Calendar grant
th doctor --setup-imessage                         # macOS: set up the `imessage` tool (th-1665ed)
th doctor --setup-reminders                        # macOS: drive the Reminders grant (th-94cc4a)
th cache list / prune / clear
th service install / start / stop / status         # run smooth as a background daemon
```

#### What `th doctor` raises (pearl th-ba764e)

Besides the health checks (daemon, Dolt, providers, `~/.smooth`, pearls, backup,
workspace volume, git hooks) a bare `th doctor` now reports two more things:

- **Smoo AI sign-in** — whether a user or M2M session exists for the active auth
  profile (`th auth login` if not). Not a health failure: a local-only Big Smooth
  works without it, so it's raised as a *setup step*, not an issue.
- **A `macOS access` section** — the grants Big Smooth's personal-data tools
  need, which used to be visible only behind the `--setup-*` flags. Before this,
  a bare `th doctor` could say "all checks passed" on a Mac where the calendar,
  reminders and messages tools were all dead.

  ```
    macOS access
      grants belong to Big Smooth.app; `th`'s own probe is a proxy, not proof
      ✓ Big Smooth.app: /Users/you/Applications/Big Smooth.app
      ✓ ical CLI: /opt/homebrew/bin/ical
      ○ Calendar: not-determined — nobody has asked yet
        → th doctor --setup-calendar (or Big Smooth's Set Up menu)
      ○ Reminders: not-determined — nobody has asked yet
        → th doctor --setup-reminders (or Big Smooth's Set Up menu)
      ✓ Messages: chat.db readable (Full Disk Access granted)
  ```

  **The load-bearing nuance: the process that needs these grants is the daemon
  (`Big Smooth.app`), not `th`.** TCC grants are per-binary, so what `th` reads
  about *itself* (the EventKit statuses, `chat.db` readability) is a proxy — it
  never proves the app bundle is granted, which is why every line says "for
  `th`" and the fix always names the app. The one real check is Messages:
  `~/Library/Messages/chat.db` is Full-Disk-Access-gated, so a successful read
  means FDA is genuinely in place for the probing binary.

The run ends with a setup-step summary — `N setup step(s) not ready: …` plus
`Walk them all: th doctor --onboard` — kept separate from the `N issue(s) found`
health count.

#### `th doctor --onboard`

The guided first-run flow, and the CLI backbone Big Smooth.app's **Set Up** menu
and the daemon's first-run onboarding shell out to (so the sequence lives in one
place). It runs the full health check, then walks every step that came back not
ready, in dependency order:

1. **LLM provider credentials** → points at `th model login <provider>`
2. **Smoo AI sign-in** → points at `th auth login`
3. **Full Disk Access** → runs `--fix-fda`
4. **Calendar** → runs `--setup-calendar`
5. **Reminders** → runs `--setup-reminders`
6. **Messages** → runs `--setup-imessage`

Ready steps are skipped, so re-running it is cheap and idempotent. A step that
fails is reported with its manual command and the walk continues — one broken
step never strands the rest. The two credential steps are interactive flows of
their own, so onboarding points at them rather than hijacking the terminal.

Run it on the Mac's console: like every `--setup-*` flag, the macOS prompts never
appear over SSH. Verify afterwards with a plain `th doctor`.

#### `th doctor --setup-calendar` (macOS)

Makes Big Smooth's `calendar` tool work out of the box (pearl th-94cc4a). Two
things have to be true; this command does the first and drives the second:

1. **The `ical` CLI exists.** Side-loaded from the
   [BRO3886/ical](https://github.com/BRO3886/ical) release to `~/.smooth/bin/ical`
   — no Homebrew tap needed (`curl` + `tar` only). The daemon's tool resolves
   `SMOOTH_ICAL_BIN` → `~/.smooth/bin/ical` → `/opt/homebrew/bin/ical` → `PATH`.
2. **macOS has granted Big Smooth.app Calendar access.** A TCC grant belongs to
   the **app bundle** that asks, so `th` cannot request it — it launches (or
   tells you to restart) `Big Smooth.app`, which calls
   `EKEventStore.requestFullAccessToEvents` at startup and makes the OS prompt
   appear. **Click Allow.** The prompt only shows in a GUI login session on the
   Mac itself — never over SSH — and macOS asks exactly once; after a denial it
   must be re-enabled in System Settings → Privacy & Security → Calendars.

Until both hold, the `calendar` tool still registers and answers every call with
"run `th doctor --setup-calendar`" instead of failing opaquely, so Big Smooth can
tell you what to do rather than claim it has no calendar.

Install Big Smooth.app first if it's missing: `scripts/macos/install-local.sh`.

Once set up, Big Smooth can both read and **adjust** the calendar: `today`,
`upcoming`, `list`, `search`, `show`, `calendars`, `free`, `inbox`, plus `add`
and `update`. Cancelling lives on a **second tool**, `calendar_delete`, which
**asks you to confirm** before it runs — the one calendar mutation that can't be
undone on the next turn gets a prompt (approve/deny in the web UI); everything
else stays unprompted. `update`/`calendar_delete` require an event id (get one
from a read) — without it `ical` would open an interactive picker the daemon
can't answer. Same reason `-i` is refused and `delete` is always run with
`--force`: the decision point is the confirmation prompt, not a TTY.

#### `th doctor --setup-reminders` (macOS)

Makes Big Smooth's `reminders` tool work — the second slice of pearl th-94cc4a.
Nothing to install: reminders are read and written through **EventKit
in-process** (there is no reminders equivalent of the `ical` CLI). The only
prerequisite is the grant, and Reminders is a **separate** TCC grant from
Calendar — `--setup-calendar` does nothing for it. Same mechanics as the calendar
grant: `th` can't ask (a TCC grant belongs to the app bundle), so this launches
or tells you to restart `Big Smooth.app`, which calls
`EKEventStore.requestFullAccessToReminders` at startup. **Click Allow.** GUI
login session only, asked exactly once; after a denial, re-enable it in System
Settings → Privacy & Security → Reminders.

Until then the `reminders` tool still registers and answers every call with "run
`th doctor --setup-reminders`" — better than reporting an empty todo list.

Once granted, Big Smooth can read and adjust the user's real Reminders:

| Verb | Arguments | Does |
|---|---|---|
| `list` | `status` (`open` default / `all`), `list` (filter by list name) | reads reminders |
| `add` | `title` (required), `due`, `list` | creates one |
| `complete` | `id` (from a `list`) | marks it done |

Due dates are **absolute** — `YYYY-MM-DD` or `YYYY-MM-DD HH:MM`. Natural language
("tomorrow 2pm") is deliberately refused: the model resolves relative dates with
the `current_datetime` tool first, rather than a half-working parser booking
things on the wrong day. There is **no delete verb** — the reversible answer to a
reminder the agent shouldn't have made is completing it.

`--setup-imessage` reports whether `~/Library/Messages/chat.db` is readable (Full
Disk Access) and fires a harmless Apple Event at Messages.app so the one-time
**Automation** prompt appears now rather than mid-turn. Neither grant can be set
programmatically (the TCC database is SIP-protected), so both are detect-and-guide
— and both only prompt in a **GUI login session on the Mac itself**, never over
SSH.

Once granted, Big Smooth's `imessage` tool can read, search and **send** the
user's real Messages: `recent`, `thread`, `search`, `conversations`, `send`.
Reading exposes the whole message history to the model — a deliberate opt-in.
Revoke by removing Big Smooth from Full Disk Access. See
[Security-Model](../Architecture/Security-Model.md) for why it runs outside the
kernel sandbox and what bounds the exposure.

### LLM cast

```bash
th cast models                                     # list groups from the configured provider via GET /v1/models
                                                   # (also folds in any configured local provider's live models)
```

`th cast models` also surfaces **extension-registered providers** (SEP Phase 7):
any globally installed extension (`~/.smooth/extensions/`) that registers an LLM
provider is loaded headlessly and its declared models are listed under an
`extension <ext>.<provider>` section (and, in `--json`, as `<provider>/<model>`
ids). Project extensions are never spawned by this command. Extensions register
providers via the SEP `registerProvider` surface; the host proxies completions to
them over `provider/complete` with streamed `provider/delta` chunks.

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

# White-label a partner org
th branding from-url https://partner.example                        # dry run
th branding set --logo ./logo.png --primary '#7c3aed'
th branding enable                                                  # refuses on bad contrast

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
