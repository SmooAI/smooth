---
'smooai-smooth-daemon': patch
---

Phase 2 (EPIC th-c89c2a): wire durable memory into the agent loop. `AppState`
now carries an `Arc<dyn Memory>` (the SQLite-backed store in `persistent`,
in-memory in `new`), threaded into `run_task` and attached via
`AgentConfig::with_memory`. The engine then auto-recalls relevant entries for
each user message and injects them (with a freshness nudge for
Project/Reference types) ahead of the prompt — so a cross-session fact stored
once is recalled on later turns and across restarts. `run_task`'s growing
parameter list is bundled into a `RunDeps` struct. Follow-up: a `remember`
tool / extraction step so the agent populates its own memory. Tested:
persistent state carries durable, recallable memory across a restart.
