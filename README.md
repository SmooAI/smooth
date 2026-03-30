<div align="center">

# Smooth

**AI Agent Orchestration Platform**

A local-first, general-purpose AI agent orchestration system that coordinates multiple agents to work on any project through a structured leader/worker model with adversarial review.

[![TypeScript](https://img.shields.io/badge/TypeScript-5.8-3178C6?logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![Next.js](https://img.shields.io/badge/Next.js-16-000000?logo=next.js&logoColor=white)](https://nextjs.org/)
[![React](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=black)](https://react.dev/)
[![LangGraph](https://img.shields.io/badge/LangGraph-1.2-1C3C3C?logo=langchain&logoColor=white)](https://langchain-ai.github.io/langgraphjs/)
[![Docker](https://img.shields.io/badge/Docker-Compose-2496ED?logo=docker&logoColor=white)](https://docs.docker.com/compose/)
[![Tailscale](https://img.shields.io/badge/Tailscale-Forward-4C566A?logo=tailscale&logoColor=white)](https://tailscale.com/)
[![PostgreSQL](https://img.shields.io/badge/PostgreSQL-16-4169E1?logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![Drizzle](https://img.shields.io/badge/Drizzle-ORM-C5F74F?logo=drizzle&logoColor=black)](https://orm.drizzle.team/)

</div>

---

## Architecture

```
                    ┌─────────────────────────────────┐
                    │         User Interfaces          │
                    │  ┌─────────┐    ┌────────────┐  │
                    │  │ th CLI  │    │  Next.js   │  │
                    │  │ (Ink)   │    │  Web App   │  │
                    │  └────┬────┘    └─────┬──────┘  │
                    └───────┼───────────────┼─────────┘
                            │               │
                     ┌──────┴───────────────┴──────┐
                     │     Leader (Hono + LangGraph)│
                     └──────┬───────────────┬──────┘
                            │               │
                   ┌────────┴──┐     ┌──────┴────────┐
                   │ PostgreSQL│     │  Docker API   │
                   └───────────┘     └───────┬───────┘
                          ┌──────────────────┼──────────┐
                    ┌─────┴─────┐    ┌──────┴──────┐   │
                    │  Worker 1 │    │  Worker 2   │  ...
                    │ (OpenCode)│    │ (OpenCode)  │
                    └───────────┘    └─────────────┘
```

**Leader** (custom LangGraph service) orchestrates **Workers** (OpenCode in Docker sandboxes). All durable state flows through **Beads**. Communication via Beads-backed messaging. Adversarial review on every task.

## Tech Stack

| Layer | Technology |
|---|---|
| **Orchestration** | LangGraph (TypeScript), custom leader node |
| **Workers** | OpenCode (Zen subscription), Docker sandboxes |
| **State** | Beads (durable SoR), PostgreSQL (Drizzle ORM) |
| **Web** | Next.js 16, React 19, Tailwind CSS 4, Shadcn UI, AI SDK Elements |
| **CLI/TUI** | React Ink 6, @inkjs/ui, Commander.js |
| **Auth** | Better Auth (sessions + API keys), Tailscale identity headers |
| **API** | Hono, Zod validation, SSE streaming |
| **Networking** | Tailscale (Serve, MagicDNS, Tags, ACLs) |
| **Toolchain** | pnpm, Turborepo, oxlint, oxfmt, tsgo, Vitest |

## Monorepo Structure

```
smooth/
├── apps/
│   └── web/                    # Next.js 16 web interface
├── packages/
│   ├── leader/                 # LangGraph orchestration service
│   ├── cli/                    # `th` CLI + React Ink TUI
│   ├── shared/                 # Shared types + Zod schemas
│   ├── db/                     # Drizzle ORM schemas
│   ├── auth/                   # Better Auth config
│   ├── tools/                  # Custom MCP tools for workers
│   └── smoo-api/               # SmooAI platform API client
├── docker/
│   ├── docker-compose.yml      # Full stack deployment
│   ├── leader/                 # Leader Dockerfile
│   ├── postgres/               # Init, backup, restore
│   └── worker/                 # OpenCode worker image
└── .beads/                     # Beads issue tracking
```

## Prerequisites

- Node.js 24+ (see `.nvmrc`)
- pnpm 10.6+
- Docker & Docker Compose
- Tailscale (optional, for private networking)

## Getting Started

```bash
# Clone
git clone git@github.com:SmooAI/smooth.git
cd smooth

# Install dependencies
pnpm install

# Start PostgreSQL
pnpm docker:up

# Start development
pnpm dev
```

## The `th` CLI

```bash
th                          # Quick status
th tui                      # Full terminal UI
th web                      # Open web interface
th status                   # System health
th project create <name>    # Create project
th run <bead-id>            # Trigger work on bead
th approve <bead-id>        # Approve review
th inbox                    # Pending messages
th workers                  # Active workers
th jira sync                # Sync with Jira
th smoo agents              # List SmooAI agents
th db backup                # Backup PostgreSQL
th tailscale status         # Tailscale node status
```

## Worker Lifecycle

Every task follows a structured lifecycle with adversarial review:

```
ASSESS → PLAN → ORCHESTRATE → EXECUTE → FINALIZE → REVIEW
```

- **Assess**: Inspect bead context, graph neighbors, previous work
- **Plan**: Define bounded steps, tools, expected outputs
- **Orchestrate**: Coordinate sub-work, spawn child beads
- **Execute**: Work inside Docker sandbox with MCP tools
- **Finalize**: Summarize, link artifacts, identify unlocked work
- **Review**: Adversarial review worker inspects diffs, tests, artifacts

## Development

```bash
pnpm build              # Build all packages
pnpm test               # Run all tests
pnpm typecheck          # Type check
pnpm lint               # Lint with oxlint
pnpm format             # Format with oxfmt
pnpm check-all          # Full CI check
```

## Git Workflow

All feature work happens in git worktrees:

```bash
# Create worktree
git worktree add ../smooth-SMOODEV-XX-desc -b SMOODEV-XX-desc main

# Work, commit, then merge from main worktree
cd ~/dev/smooai/smooth
git merge SMOODEV-XX-desc --no-ff && git push
```

## License

Private - Smoo AI
