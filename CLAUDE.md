# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

**Use Context7 MCP server for up-to-date library documentation.**

> **CRITICAL: All feature work MUST happen in a git worktree.** Never edit source code or commit directly on `main` in `~/dev/smooai/smooth/`. The main worktree stays on `main` and is only used for merging, pulling, and creating new worktrees. A `PreToolUse` hook enforces this — see `.claude/hooks/enforce-worktree.sh`.

## Project Overview

Smooth is the Smoo AI CLI and orchestration platform. It coordinates multiple AI agents (Smooth Operators in Microsandbox microVMs) to work on any project through a structured leader/worker model with adversarial security review. Beads is the durable system of record.

---

## 1. Issue Tracking — Beads + Jira Integration

**Philosophy**: Beads tracks local work context and dependencies. Jira (SMOODEV project) is the external source of truth for project management.

### Beads has built-in Jira sync

Configure once (already done for this repo):

```bash
bd config set jira.url "https://smooai.atlassian.net"
bd config set jira.project "SMOODEV"
bd config set jira.username "$JIRA_EMAIL"
# API token from JIRA_API_TOKEN env var (or bd config set jira.api_token)
```

### Creating work

1. **Create a Jira ticket first** via REST API or Jira UI
2. **Create a matching beads issue**:
    ```bash
    bd create --title="SMOODEV-XX: Title" --description="What and why" --type=task --priority=2 --add-label=<label>
    ```
3. **Or use `bd jira sync --pull`** to import Jira issues into beads automatically.

### Beads quick reference

```bash
bd ready                              # Show issues ready to work on
bd list --status=open                 # All open issues
bd list --status=in_progress          # Active work
bd show <id>                          # Issue details with dependencies
bd update <id> --status=in_progress   # Claim work
bd close <id1> <id2> ...              # Close completed issues
bd dep add <issue> <depends-on>       # Add dependency
bd blocked                            # Show blocked issues
bd sync                               # Sync with git remote
bd jira sync                          # Bidirectional Jira sync
```

### Available labels

`backend`, `cli`, `db`, `frontend`, `hooks`, `infra`, `leader`, `operator`, `security`, `testing`, `tools`, `tui`, `web`, `websocket`

---

## 2. Git Workflow — Worktrees, Branches, Merging

### Working directory structure

All work happens from `~/dev/smooai/`. The main worktree is at `~/dev/smooai/smooth/`. Feature worktrees live alongside it:

```
~/dev/smooai/
├── smooth/                              # Main worktree (ALWAYS on main, kept up to date)
├── smooth-SMOODEV-33-operator-cli/      # Feature worktree
├── smooth-SMOODEV-45-test-coverage/     # Feature worktree
└── ...
```

**IMPORTANT:** `~/dev/smooai/smooth/` must ALWAYS stay on the `main` branch and be kept up to date. **Never do feature work directly on main.** All feature work goes in worktrees. After merging a feature branch, always `git pull --rebase` in the main worktree to keep it current.

### Branch naming

Always prefix with the Jira ticket number: `SMOODEV-XX-short-description`

### Commit messages

Always prefix with the Jira ticket. Explain **why**, not just what: `SMOODEV-XX: Add survey validation to prevent duplicate submissions`

### Worktree workflow (MANDATORY for all feature work)

```bash
# Create worktree from main
cd ~/dev/smooai/smooth
git worktree add ../smooth-SMOODEV-XX-short-desc -b SMOODEV-XX-short-desc main

# Work in the worktree
cd ../smooth-SMOODEV-XX-short-desc
pnpm install
```

### Merging to main

```bash
cd ~/dev/smooai/smooth
git checkout main && git pull --rebase
git merge SMOODEV-XX-short-desc --no-ff
git push
```

### Cleanup

```bash
git worktree remove ~/dev/smooai/smooth-SMOODEV-XX-short-desc
git branch -d SMOODEV-XX-short-desc
```

---

## 3. Project Structure

```
smooth/
├── apps/web/               # Next.js 16 web interface (Tailwind CSS 4)
├── packages/
│   ├── leader/             # LangGraph orchestration + Hono API + WebSocket
│   │   └── src/backend/    # Pluggable ExecutionBackend (Microsandbox, future Lambda)
│   ├── cli/                # `th` CLI + React Ink TUI (23 commands)
│   ├── shared/             # Shared types, Zod schemas, audit logging
│   ├── db/                 # Drizzle ORM (SQLite at ~/.smooth/smooth.db)
│   ├── auth/               # Better Auth (sessions + API keys)
│   ├── tools/              # 12 operator tools + 3 guardrail hooks
│   └── smoo-api/           # SmooAI platform M2M API client
├── docker/worker/          # Smooth Operator OCI image
└── .beads/                 # Issue tracking (Jira-synced)
```

### Key Technologies

- **Orchestration**: LangGraph (TypeScript), custom leader node
- **Smooth Operators**: OpenCode, Microsandbox microVMs (hardware isolation)
- **State**: Beads (durable SoR), SQLite (Drizzle ORM)
- **Web**: Next.js 16, React 19, Tailwind CSS 4
- **CLI/TUI**: React Ink 6, Commander.js
- **Auth**: Multi-provider LLM auth, Better Auth, Tailscale identity
- **API**: Hono, WebSocket (real-time events + steering), Zod validation
- **Config**: @smooai/config integration for schema management
- **Networking**: Tailscale (Serve, MagicDNS, Tags, ACLs)
- **Toolchain**: pnpm, Turborepo, oxlint, oxfmt, Vitest, Changesets

---

## 4. Build, Test, and Development Commands

```bash
pnpm install              # Install all dependencies
pnpm build                # Build all packages
pnpm test                 # Run all tests (48 passing)
pnpm typecheck            # TypeScript type checking
pnpm lint                 # oxlint
pnpm lint:fix             # oxlint --fix
pnpm format               # oxfmt
pnpm format:check         # oxfmt --check
pnpm check-all            # Full CI check
pnpm pre-commit-check     # Pre-commit validation (sync + lint + typecheck + test + format)
```

### Starting Smooth

```bash
th up                     # Start everything (auto-installs Microsandbox if needed)
th down                   # Stop leader + optionally msb server
th status                 # System health check
```

No Docker required. SQLite auto-creates at `~/.smooth/smooth.db`. Microsandbox auto-installs on first `th up`.

### Package-specific

```bash
pnpm --filter @smooai/smooth-leader test
pnpm --filter @smooai/smooth-shared build
pnpm --filter @smooai/smooth-db generate      # Drizzle migration
pnpm --filter @smooai/smooth-db migrate        # Apply migration
```

---

## 5. Coding Style

- 4-space indentation, 160-character line width
- oxfmt for formatting, oxlint for linting
- Single quotes, trailing commas, bracket spacing
- Packages and directories: kebab-case
- Components: PascalCase
- Hooks: useCamelCase
- Zod for all API validation and structured output
- Drizzle ORM for database access
- Let errors propagate to global handler (no unnecessary try/catch)

---

## 6. Testing

- Vitest for unit/integration tests
- Colocated as `*.test.ts`
- Every batch of work MUST include tests
- All tests must pass before merging

---

## 7. Changesets

Always add changesets when `@smooai/smooth-*` packages change (including private packages):

```bash
pnpm changeset
```

---

## 8. Data

All Smooth state lives at `~/.smooth/`:

- **SQLite database**: `~/.smooth/smooth.db` — leader memory, worker runs, auth, config
- **Audit logs**: `~/.smooth/audit/` — rotating tool usage logs per operator
- **Providers**: `~/.smooth/providers.json` — LLM provider credentials
- **Artifacts**: `~/.smooth/artifacts/` — operator work output
- **Config**: `~/.smooth/config.json` — CLI settings
- **Backup**: `th db backup` copies the SQLite file
- **Clean reset**: `rm -rf ~/.smooth` starts fresh

---

## 9. Pre-Push Code Review

Before merging, review all changes as an SME:

```bash
git diff main...HEAD
```

Check: security, code quality, test coverage, best practices. Fix issues, don't just note them.

---

## 10. Landing the Plane

When ending a work session, complete ALL steps:

1. Run `pnpm pre-commit-check` — must pass
2. Add changesets when `@smooai/smooth-*` packages changed
3. Close beads issues: `bd close <id1> <id2> ...`
4. Push to remote: `git push`
5. Verify CI green: `gh run list`
