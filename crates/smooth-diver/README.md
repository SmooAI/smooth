<div align="center">

# smooth-diver

**The Pearl Diver**

*Owns every pearl in the ocean. Dispatches them, tracks them through assess → plan → execute → review, records what they cost, and keeps Jira honest. The one on the Board who knows what everyone's working on.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-diver)](https://crates.io/crates/smooai-smooth-diver)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Diver is the pearl-lifecycle service in Smooth's Boardroom. Big Smooth calls `POST /dispatch` with a task; Diver creates a pearl via [`smooth-pearls`](https://crates.io/crates/smooai-smooth-pearls), records the dispatch, and returns the id. When the operator finishes, `POST /complete/:id` closes the pearl, persists the agent's session messages, records token + VM costs, and (if Jira is wired) pushes status back to the source ticket.

The full work model lives in Diver:

- **Pearl graph** — parent-child, dependencies, labels, comments, rolling history.
- **Cost accounting** — per-tool-call `CostEntry` records (model tokens, VM seconds, storage). Roll-ups per pearl, operator, phase, or project.
- **Jira sync** — env-driven (`JIRA_URL`, `JIRA_USER`, `JIRA_TOKEN`). `JiraClient::pull` imports open tickets as pearls; `push` mirrors pearl state + closing notes back.
- **Session replay** — every agent message is a `SessionMessage` with trace context, so a pearl can be resurrected and continued from its last checkpoint.

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`AppState`** / **`build_router`** — axum router for `/dispatch`, `/complete/:id`, `/costs`, `/sessions/:id`.
- **`DispatchRequest`** / **`DispatchResult`** — wire types for the dispatch entrypoint.
- **`DiverStore`** — persistence facade over `PearlStore` plus session messages and cost entries.
- **`JiraClient`** — REST client with `pull` / `push` helpers.

## Usage

```rust
use smooth_diver::{build_router, AppState};

let state = AppState::new_with_store(store);
let app = build_router(state);
axum::serve(listener, app).await?;
```

## License

MIT
