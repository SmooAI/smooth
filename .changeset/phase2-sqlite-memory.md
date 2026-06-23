---
'smooai-smooth-daemon': patch
---

Phase 2 (EPIC th-c89c2a): add durable, cross-session agent memory. A new
`SqliteMemory` implements the engine's synchronous `Memory` trait against a
`memories` table (sharing the daemon's existing SQLite connection), so an
always-on agent's recall survives restarts — the hermes-style memory the
in-memory engine backend can't persist. `store`/`forget` are direct
row ops; `recall` mirrors `InMemoryMemory`'s keyword scoring (fraction of
query words found in the content, highest first), with `MemoryType` +
metadata round-tripped as JSON. Exposed on `Stores.memory`. Wiring it into
the agent loop (`AgentConfig::with_memory`) is the next slice. Tested:
persist-across-reopen, keyword recall ordering, metadata round-trip, empty
query, and forget.
