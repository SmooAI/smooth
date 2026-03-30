<div align="center">

# Smooth

**AI Agent Orchestration Platform**

A local-first, general-purpose AI agent orchestration system that coordinates Smooth Operators to work on any project through a structured leader/worker model with adversarial review.

[![npm](https://img.shields.io/npm/v/@smooai/smooth-cli?label=@smooai/smooth-cli&color=06b6d4)](https://www.npmjs.com/package/@smooai/smooth-cli)
[![TypeScript](https://img.shields.io/badge/TypeScript-5.8-3178C6?logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![Next.js](https://img.shields.io/badge/Next.js-16-000000?logo=next.js&logoColor=white)](https://nextjs.org/)
[![LangGraph](https://img.shields.io/badge/LangGraph-1.2-1C3C3C?logo=langchain&logoColor=white)](https://langchain-ai.github.io/langgraphjs/)
[![SQLite](https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white)](https://sqlite.org/)
[![Microsandbox](https://img.shields.io/badge/Microsandbox-microVM-FF6B6B)](https://microsandbox.dev/)
[![Tailscale](https://img.shields.io/badge/Tailscale-Forward-4C566A?logo=tailscale&logoColor=white)](https://tailscale.com/)

</div>

---

## Install

```bash
npm install -g @smooai/smooth-cli
```

## Quick Start

```bash
# Authenticate with your LLM provider
th auth login anthropic --api-key sk-ant-...

# Start Smooth
th up

# Open the terminal UI
th tui
```

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

**Leader** orchestrates **Smooth Operators** (OpenCode agents in Microsandbox microVMs). All durable state flows through **Beads**. Adversarial review on every task.

**Zero external dependencies.** No Docker. No PostgreSQL. Just Node.js.

## The `th` CLI

```bash
# Auth — multi-provider (Anthropic, OpenAI, OpenRouter, Groq, Google, OpenCode Zen)
th auth login <provider>        # Add provider credentials
th auth providers               # List configured providers
th auth status                  # Full auth overview

# Orchestration
th up                           # Start Smooth (auto-installs Microsandbox)
th down                         # Stop Smooth
th status                       # System health
th tui                          # Full terminal UI
th web                          # Open web interface

# Work
th project create <name>        # Create project
th run <bead-id>                # Trigger work on bead
th approve <bead-id>            # Approve review
th inbox                        # Pending messages
th operators                    # Active Smooth Operators

# SmooAI Platform
th smoo config push/pull/set/get/list/diff  # Config schema management
th smoo agents                  # List SmooAI agents
th jira sync                    # Sync with Jira

# System
th db status                    # Database info
th db backup                    # Backup SQLite
th config show/set              # Local settings
th tailscale status             # Tailscale node status
```

## Tech Stack

| Layer | Technology |
|---|---|
| **Orchestration** | LangGraph (TypeScript), custom leader node |
| **Smooth Operators** | OpenCode, Microsandbox microVMs |
| **State** | Beads (durable SoR), SQLite (Drizzle ORM) |
| **Web** | Next.js 16, React 19, Tailwind CSS 4 |
| **CLI/TUI** | React Ink 6, Commander.js |
| **Auth** | Multi-provider (same as OpenCode), Better Auth, Tailscale identity |
| **API** | Hono, Zod validation, SSE streaming |
| **Config** | @smooai/config integration for schema management |
| **Networking** | Tailscale (Serve, MagicDNS, Tags, ACLs) |

## Packages

| Package | npm | Description |
|---|---|---|
| `@smooai/smooth-cli` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-cli?label=)](https://www.npmjs.com/package/@smooai/smooth-cli) | `th` CLI binary + React Ink TUI |
| `@smooai/smooth-leader` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-leader?label=)](https://www.npmjs.com/package/@smooai/smooth-leader) | LangGraph orchestration service |
| `@smooai/smooth-shared` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-shared?label=)](https://www.npmjs.com/package/@smooai/smooth-shared) | Shared types + Zod schemas |
| `@smooai/smooth-db` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-db?label=)](https://www.npmjs.com/package/@smooai/smooth-db) | SQLite via Drizzle ORM |
| `@smooai/smooth-tools` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-tools?label=)](https://www.npmjs.com/package/@smooai/smooth-tools) | MCP tools for Smooth Operators |
| `@smooai/smooth-auth` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-auth?label=)](https://www.npmjs.com/package/@smooai/smooth-auth) | Better Auth (SQLite) |
| `@smooai/smooth-smoo-api` | [![npm](https://img.shields.io/npm/v/@smooai/smooth-smoo-api?label=)](https://www.npmjs.com/package/@smooai/smooth-smoo-api) | SmooAI M2M API client |

## Smooth Operator Lifecycle

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

Pluggable `ExecutionBackend` interface — orchestration never touches VM internals.

| Backend | Status | Use |
|---|---|---|
| `local-microsandbox` | **Default** | Local dev. Hardware-isolated microVMs. |
| `aws-lambda` | Future | Hosted customer workloads. |

## Development

```bash
git clone https://github.com/SmooAI/smooth.git
cd smooth
pnpm install
pnpm build
pnpm test               # 30 tests
pnpm typecheck
pnpm lint
```

## License

MIT - [Smoo AI](https://smoo.ai)
