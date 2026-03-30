# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

**Use Context7 MCP server for up-to-date library documentation.**

> **CRITICAL: All feature work MUST happen in a git worktree.** Never edit source code or commit directly on `main` in `~/dev/smooai/smooth/`. The main worktree stays on `main` and is only used for merging, pulling, and creating new worktrees. A `PreToolUse` hook enforces this.

## Project Overview

Smooth is a local-first, general-purpose AI agent orchestration platform. It coordinates multiple AI agents (OpenCode workers in Docker sandboxes) to work on any project through a structured leader/worker model with adversarial review. Beads is the durable system of record.

---

## 1. Git Workflow — Worktrees

### Working directory structure

```
~/dev/smooai/
├── smooth/                              # Main worktree (ALWAYS on main)
├── smooth-SMOODEV-XX-short-desc/        # Feature worktree
└── ...
```

### Branch naming

Always prefix with Jira ticket: `SMOODEV-XX-short-description`

### Commit messages

Always prefix with Jira ticket, explain why: `SMOODEV-XX: Add validation to prevent duplicate submissions`

### Creating a worktree

```bash
cd ~/dev/smooai/smooth
git worktree add ../smooth-SMOODEV-XX-desc -b SMOODEV-XX-desc main
cd ../smooth-SMOODEV-XX-desc
pnpm install
```

### Merging to main

```bash
cd ~/dev/smooai/smooth
git checkout main && git pull --rebase
git merge SMOODEV-XX-desc --no-ff
git push
```

### Cleanup

```bash
git worktree remove ~/dev/smooai/smooth-SMOODEV-XX-desc
git branch -d SMOODEV-XX-desc
```

---

## 2. Project Structure

```
smooth/
├── apps/web/               # Next.js 16 web interface
├── packages/
│   ├── leader/             # LangGraph orchestration service (Hono HTTP)
│   ├── cli/                # `th` CLI + React Ink TUI
│   ├── shared/             # Shared types + Zod schemas
│   ├── db/                 # Drizzle ORM schemas + client
│   ├── auth/               # Better Auth (sessions + API keys)
│   ├── tools/              # Custom MCP tools for workers
│   └── smoo-api/           # SmooAI platform M2M API client
├── docker/
│   ├── docker-compose.yml  # PostgreSQL + services
│   ├── leader/             # Leader Dockerfile
│   ├── postgres/           # Init, backup, restore scripts
│   └── worker/             # OpenCode worker Dockerfile
└── .beads/                 # Issue tracking (gitignored data)
```

### Key Technologies

- **Orchestration**: LangGraph (TypeScript), custom leader node
- **Workers**: OpenCode (Zen), Docker sandboxes
- **State**: Beads (durable SoR), PostgreSQL (Drizzle ORM)
- **Web**: Next.js 16, React 19, Tailwind CSS 4, Shadcn UI, AI SDK Elements
- **CLI/TUI**: React Ink 6, @inkjs/ui, Commander.js
- **Auth**: Better Auth + Tailscale identity headers
- **API**: Hono, Zod validation, SSE streaming
- **Networking**: Tailscale (Serve, MagicDNS, Tags, ACLs)
- **Toolchain**: pnpm, Turborepo, oxlint, oxfmt, tsgo, Vitest, Changesets

---

## 3. Build, Test, and Development Commands

```bash
pnpm install              # Install all dependencies
pnpm dev                  # Start development
pnpm build                # Build all packages
pnpm test                 # Run all tests
pnpm typecheck            # TypeScript type checking
pnpm lint                 # oxlint
pnpm lint:fix             # oxlint --fix
pnpm format               # oxfmt
pnpm format:check         # oxfmt --check
pnpm check-all            # Full CI check
pnpm pre-commit-check     # Pre-commit validation
```

### Docker

```bash
pnpm docker:up            # Start PostgreSQL
pnpm docker:down          # Stop (preserves data)
pnpm docker:logs          # Stream logs
pnpm docker:ps            # Container status
```

**NEVER use `docker compose down -v`** — this destroys the PostgreSQL data volume. Use `th db backup` first.

### Package-specific

```bash
pnpm --filter @smooth/leader test
pnpm --filter @smooth/shared build
pnpm --filter @smooth/db generate      # Drizzle migration
pnpm --filter @smooth/db migrate       # Apply migration
```

---

## 4. Coding Style

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

## 5. Testing

- Vitest for unit/integration tests
- Colocated as `*.test.ts`
- Every batch of work MUST include tests
- All tests must pass before merging

---

## 6. Changesets

Always add changesets when `@smooth/*` packages change (including private packages):

```bash
pnpm changeset
```

---

## 7. PostgreSQL Data Protection

The PostgreSQL volume (`smooth-pgdata`) contains leader memory, checkpoints, and auth data.

- **Named volume**: Survives `docker compose down`
- **Backup**: `bash docker/postgres/backup.sh`
- **Restore**: `bash docker/postgres/restore.sh <file>`
- **NEVER** run `docker compose down -v` or `docker volume rm smooth-pgdata`

---

## 8. Pre-Push Code Review

Before merging, review all changes as an SME:

```bash
git diff main...HEAD
```

Check: security, code quality, test coverage, best practices. Fix issues, don't just note them.
