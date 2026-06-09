# Using smooth-operator & smooth-operator-agent to their fullest in smooth

This guide explains how the **smooth** monorepo should get maximum leverage from the two public OSS projects that grew out of it:

| Project | What it is | Repo |
| --- | --- | --- |
| **smooth-operator** (engine) | The Rust agent-orchestration **engine** — `Agent`, `Workflow`, `Tool`, `CheckpointStore`, `LlmProvider`, `Memory`, `KnowledgeBase`, HITL, cost. Published as `smooai-smooth-operator-core` on crates.io; smooth consumes it as `smooth-operator` (package-aliased). | [SmooAI/smooth-operator-core](https://github.com/SmooAI/smooth-operator-core) (extracted from the former in-tree `crates/smooth-operator`) |
| **smooth-operator-agent** | The productized, polyglot knowledge-chat + tools + conversations **service** built on the engine. Serverless (SST/AWS) or k8s. | [SmooAI/smooth-operator-agent](https://github.com/SmooAI/smooth-operator-agent) |

> TL;DR: smooth **already runs on** smooth-operator (the `th` TUI, Big Smooth, coding workflows, the cast/role system). The upside is to (1) consume it as the **extracted public crate** instead of vendoring, (2) wire the **real backends** behind its trait seams, and (3) dogfood **smooth-operator-agent** as smooth's own hosted knowledge assistant.

## 1. Consume the extracted crate (done — published on crates.io)

The engine extraction is **complete**. smooth no longer vendors the engine: the in-tree `crates/smooth-operator` copy was deleted and smooth depends on the published **`smooai-smooth-operator-core`** crate (repo [SmooAI/smooth-operator-core](https://github.com/SmooAI/smooth-operator-core)). The dep key stays `smooth-operator` (package-aliased back to `smooth_operator`) so every `use smooth_operator::…` import for the generic engine API is unchanged.

The cutover landed in two steps:

1. **SMOODEV-1787 (PR 1/4)** — replace the in-tree copy with a rev-pinned git dep on the engine repo.
2. **SMOODEV-1788 (PR 4/4, the final cutover)** — switch from the git rev to the **published crates.io release `smooai-smooth-operator-core = "0.14.0"`**, the clean *generic* engine. Root `Cargo.toml` now reads `smooth-operator = { version = "0.14.0", package = "smooai-smooth-operator-core" }`; `Cargo.lock` resolves it from the crates.io registry (checksum-pinned), not a git source.

This makes smooth a **consumer** of the public engine — the same artifact the rest of the world uses — so our dogfooding pressure improves the OSS product directly.

### Where the th-code harness lives now (`crates/smooth-cast`)

The published `0.14.0` engine is **generic** — it deliberately dropped the `th code` coding-harness specifics that only smooth used. Those bits were re-homed into the smooth-owned **`smooth-cast`** crate (`smooai-smooth-cast`), built on the engine's generic public `Cast`/`OperatorRole`/`Clearance` API:

- **`smooth_cast::coding_workflow`** — the `th code` single-agent outer loop (`run_coding_workflow`, `task_text_has_cleanup_intent`, …).
- **`smooth_cast::skills`** — skill discovery (`discover`, `SkillScope`, `SkillSource`, `Skill`) + the built-in `create-skill` skill.
- **`smooth_cast::cast`** — the four coding-harness cast roles the generic engine no longer ships (`fixer`, `oracle`, `chief`, `intent_classifier`), and a `cast::builtin()` that registers them on top of the engine's generic built-in roles (`tagger`/`presser`/`recapper`/`mapper`/`heckler`/`scout`/`runner`).

Consumers (`smooth-operative`, `smooth-code`, `smooth-cli`, `smooth-bigsmooth`) call `smooth_cast::cast::builtin()` wherever they need a harness role — the engine's own `smooth_operator::Cast::builtin()` only has the generic roles. The Big-Smooth reporter hooks the engine also dropped stay deleted (no smooth consumers).

See the parity epic (SMOODEV-1466) and the extraction punch-list.

## 2. Use every trait seam (don't reinvent)

smooth-operator ships abstractions that smooth components keep re-hand-rolling. Prefer the engine's:

- **CheckpointStore** — `MemoryCheckpointStore`, `FileCheckpointStore`, `SqliteCheckpointStore` (`sqlite` feature), `PostgresCheckpointStore` (`postgres` feature, landed). Use these for any resumable/long-running operator instead of bespoke state files. Goalie/scribe should sit behind the `CheckpointStore`/`Memory` traits.
- **Memory** (`InMemoryMemory` + the `Memory` trait) — short/long-term + entity/user/feedback/project/reference memory types, with freshness checks. Wire archivist behind it.
- **KnowledgeBase** (RAG seam) — currently the in-memory stub; smooth gets a real vector backend by depending on smooth-operator-agent's adapter layer (below) rather than building another retriever.
- **Workflow / WorkflowBuilder** — the graph engine. Author multi-step operator flows as `Workflow<S>` graphs with conditional edges, not ad-hoc loops.
- **HITL** (`ConfirmationHook`, `human_channel`) — gate any operator action that writes/network/shells through the confirmation channel; surface to the user via the same protocol events smooth-operator-agent uses.
- **Cost** (`CostTracker`, `CostBudget`) — enforce per-run budgets everywhere an operator calls an LLM.

## 3. Dogfood smooth-operator-agent as the "smooth assistant"

Stand up a smooth-operator-agent instance over smooth's own corpus — pearls, `docs/`, scribe transcripts, code — to give the team a knowledge-grounded assistant:

- Ingest the corpus into the agent's `KnowledgeBase` (hybrid retrieval: dense + keyword + rerank).
- Expose smooth's operations (run a coding workflow, query pearls, search bench sessions) as **tools** using the agent's tool-definition shape.
- Talk to it from any smooth component via the **polyglot protocol clients** (Rust/Go/TS) instead of bespoke HTTP — the wire protocol is schema-driven and generated from one spec.
- Run it on the **k8s deploy path** alongside Big Smooth (Postgres + pgvector), or serverless for ephemeral instances.

## 4. Close the parity gaps (they benefit smooth most)

The smooth-operator parity epic items pay off here first:

- **OpenTelemetry `gen_ai.*` conventions** — adopt in smooth-operator so Big Smooth observability and the agent service share one trace vocabulary (interops with the Microsoft Agent Framework + the smooai stack). Currently a gap.
- **Structured output** — add `response_format`/json-schema to `LlmConfig` so operators get typed results without prompt-scraping.
- **Vector-backed KnowledgeBase** — the real RAG backend (from smooth-operator-agent's Postgres/pgvector adapter) replaces the in-memory stub.

## 5. MCP both directions

smooth-operator already depends on `rmcp` (the Rust MCP SDK). Expose smooth's tools over MCP so external agents can drive them, and consume external MCP servers as tools inside smooth operators. smooth-operator-agent's tool layer should be the canonical place tools are defined once and surfaced over MCP.

## Related
- [SmooAI/smooth-operator-agent docs/ARCHITECTURE.md](https://github.com/SmooAI/smooth-operator-agent/blob/main/docs/ARCHITECTURE.md)
- [SmooAI/smooth-operator-agent docs/ROADMAP.md](https://github.com/SmooAI/smooth-operator-agent/blob/main/docs/ROADMAP.md)
- Parity epic: SMOODEV-1466
