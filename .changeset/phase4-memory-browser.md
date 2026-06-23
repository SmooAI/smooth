---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Phase 4 (EPIC th-c89c2a): add a memory browser to the control surface. A new
read-only `GET /api/memory?q=…&limit=…` searches the agent's durable memory
via the engine's keyword `recall` (no new trait surface), returning projected
hits (content, type, relevance, created_at); an empty query returns nothing.
The control surface gains a Memory search panel in the sidebar that surfaces
matching entries with their `MemoryType`. Now that the `remember` tool
populates real memories, this completes the Phase-4 operator-visibility set
(sessions, chat, approvals, status, egress, memory). Tested: recall match +
projection and the empty-query case.
