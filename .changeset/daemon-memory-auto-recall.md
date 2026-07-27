---
"@smooai/smooth": patch
---

feat(daemon): Big Smooth auto-recalls durable memories into every turn

Bumps the engine pin (`smooth-operator-server`/`-svc`) to `ce2f9e3`
(`th-daemon-memory-seam` = the core-1.7.0 daemon lineage + the cherry-picked
`StorageAdapter::memory_for_access` seam from smooth-operator #330 / th-374b27),
and overrides `memory_for_access` on the daemon's `SqliteStorageAdapter` to
return its durable SQLite memory store. The engine's runner now feeds that store
to `AgentConfig::with_memory`, so remembered preferences (the `remember` tool's
writes, th-6d1692) are auto-injected into every turn via `memory.recall(...)` —
no explicit `recall` call needed, and they survive daemon restarts. Closes the
last gap so "always add shows to smoo-hub" actually influences later turns
unprompted (th-7a9832).
