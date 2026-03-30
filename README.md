<div align="center">

# Smooth

**AI Agent Orchestration Platform**

A local-first, general-purpose AI agent orchestration system that coordinates multiple agents to work on any project through a structured leader/worker model with adversarial review.

[![TypeScript](https://img.shields.io/badge/TypeScript-5.8-3178C6?logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![Next.js](https://img.shields.io/badge/Next.js-16-000000?logo=next.js&logoColor=white)](https://nextjs.org/)
[![React](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=black)](https://react.dev/)
[![LangGraph](https://img.shields.io/badge/LangGraph-1.2-1C3C3C?logo=langchain&logoColor=white)](https://langchain-ai.github.io/langgraphjs/)
[![SQLite](https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white)](https://sqlite.org/)
[![Microsandbox](https://img.shields.io/badge/Microsandbox-microVM-FF6B6B)](https://microsandbox.dev/)
[![Tailscale](https://img.shields.io/badge/Tailscale-Forward-4C566A?logo=tailscale&logoColor=white)](https://tailscale.com/)
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
                     │   Leader (Hono + LangGraph)  │
                     └──────┬───────────────┬──────┘
                            │               │
                   ┌────────┴──┐     ┌──────┴────────┐
                   │  SQLite   │     │  Execution    │
                   │  + Beads  │     │  Backend      │
                   └───────────┘     └───────┬───────┘
                          ┌──────────────────┼──────────┐
                    ┌─────┴─────┐    ┌──────┴──────┐   │
                    │ Smooth    │    │  Smooth     │  ...
                    │ Operator 1│    │  Operator 2 │
                    │(microVM)  │    │ (microVM)   │
                    └───────────┘    └─────────────┘
```

**Leader** (custom LangGraph service) orchestrates **Smooth Operators** (OpenCode agents in Microsandbox microVMs). All durable state flows through **Beads**. Communication via Beads-backed messaging. Adversarial review on every task.

**Zero external dependencies.** No Docker. No PostgreSQL. Just Node.js and Microsandbox (auto-installed).

## Tech Stack

| Layer | Technology |
|---|---|
| **Orchestration** | LangGraph (TypeScript), custom leader node |
| **Smooth Operators** | OpenCode (Zen), Microsandbox microVMs |
| **State** | Beads (durable SoR), SQLite (Drizzle ORM) |
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
│   │   └── src/backend/        # Pluggable ExecutionBackend (Microsandbox, future Lambda)
│   ├── cli/                    # `th` CLI + React Ink TUI
│   ├── shared/                 # Shared types + Zod schemas
│   ├── db/                     # Drizzle ORM (SQLite)
│   ├── auth/                   # Better Auth config
│   ├── tools/                  # MCP tools for Smooth Operators
│   └── smoo-api/               # SmooAI platform API client
├── docker/
│   └── worker/                 # Smooth Operator OCI image
└── ~/.smooth/                  # Local state (auto-created)
    ├── smooth.db               # SQLite database
    ├── .beads/                 # Beads issue graph
    ├── artifacts/              # Operator work artifacts
    ├── config.json             # CLI config
    └── credentials.json        # API keys
```

## Prerequisites

- Node.js 24+ (see `.nvmrc`)
- pnpm 10.6+
- Tailscale (optional, for private networking)

That's it. Microsandbox is auto-installed by `th up`.

## Getting Started

```bash
# Clone and install
git clone git@github.com:SmooAI/smooth.git
cd smooth
pnpm install

# Start everything
th up
```

`th up` will:
1. Install Microsandbox if not found
2. Start the Microsandbox server
3. Auto-create SQLite database at `~/.smooth/smooth.db`
4. Start the leader service

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
th operators                # Active Smooth Operators
th jira sync                # Sync with Jira
th smoo agents              # List SmooAI agents
th db status                # Database info
th db backup                # Backup SQLite
th tailscale status         # Tailscale node status
```

## Smooth Operator Lifecycle

Every task follows a structured lifecycle with adversarial review:

```
ASSESS → PLAN → ORCHESTRATE → EXECUTE → FINALIZE → REVIEW
```

- **Assess**: Inspect bead context, graph neighbors, previous work
- **Plan**: Define bounded steps, tools, expected outputs
- **Orchestrate**: Coordinate sub-work, spawn child beads
- **Execute**: Work inside Microsandbox microVM with MCP tools
- **Finalize**: Summarize, link artifacts, identify unlocked work
- **Review**: Adversarial review operator inspects diffs, tests, artifacts

## Execution Backend

Smooth uses a pluggable `ExecutionBackend` interface. The orchestration layer never touches container/VM internals directly.

| Backend | Status | Use |
|---|---|---|
| `local-microsandbox` | **Default** | Local dev. Hardware-isolated microVMs via Microsandbox. |
| `aws-lambda` | Future | Hosted customer workloads. Lambda + S3 + DynamoDB. |

Switch backend: `SMOOTH_BACKEND=aws-lambda th up`

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
