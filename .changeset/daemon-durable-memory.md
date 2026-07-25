---
'@smooai/smooth': patch
---

Big Smooth (smooth-daemon): durable cross-session user memory (th-6d1692).

The `remember`/`recall` primitives existed but were never wired into the
always-on daemon, so it forgot everything across restarts. Now:

- **GAP 1 — tool registration.** The daemon's `SandboxedToolProvider` registers
  `RememberTool` **and** a new `RecallTool` on every turn, both sharing one
  `Arc<dyn Memory>` backend, so a fact saved in one session is retrievable in
  the next.
- **GAP 3 — durable backend.** `SqliteStorageAdapter` (the daemon's own sqlite
  store) now persists `MemoryEntry`s in the shared `kv` table under
  `entity = "memory"` — write-through on `store`/`forget`, hydrated on open —
  mirroring the existing checkpoint/conversation persistence. `memory()` hands
  out a durable `Memory` handle over the same connection. Memories survive a
  daemon restart (verified in-process AND cross-process).
- **GAP 2 — engine auto-recall: blocked upstream.** The core engine supports
  auto-recall (`AgentConfig::with_memory` → `build_context_injection` calls
  `memory.recall`), but neither `LocalServerBuilder`, `StorageAdapter`,
  `AppState`, nor the runner exposes a seam to inject a `Memory` into the
  per-turn agent (verified on the pinned rev **and** upstream `main`). Until the
  engine adds one (`StorageAdapter::memory()` → `config.with_memory(...)`), the
  new `recall` tool is the explicit read path; `serve_local_flavor` already
  holds `storage.memory()` so wiring auto-recall is a one-liner once the seam
  lands — same backend, no migration.
