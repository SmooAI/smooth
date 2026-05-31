---
'@smooai/smooth': minor
---

smooth-operator: add a `PostgresCheckpointStore` behind a new `postgres` feature (SMOODEV-1468).

Durable, Postgres-backed implementation of the existing `CheckpointStore` trait — parity with LangGraph's `PostgresSaver`, so per-`agent_id` thread state survives process restarts. Uses an r2d2 pool of synchronous `postgres` clients (the trait is sync, mirroring `SqliteCheckpointStore`/rusqlite — not async sqlx). `connect(conn_str)` builds the pool + migrates the `checkpoints` schema; `from_pool(..)` reuses a shared app pool. SQLite/in-memory stores remain the zero-dep defaults. Covered by a testcontainers integration test that spins up a throwaway Postgres and exercises the full save/load_latest/load/list/prune + upsert + agent-scoping contract.
