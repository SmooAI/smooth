---
'smooai-smooth-tools': patch
'smooai-smooth-daemon': patch
---

Phase 2 (EPIC th-c89c2a): close the memory loop with a `remember` tool. The
agent can now persist its own salient facts (`RememberTool` in smooth-tools)
— stable operator preferences, confirmed approaches, current project state,
external references — choosing the `MemoryType` so recall can apply the right
freshness treatment. The daemon registers it on every turn pointed at the same
`Memory` backend the engine recalls from, so a fact the agent remembers is
auto-surfaced on later turns and across restarts. The system prompt now tells
the agent to use it. `run_task`'s tool wiring is extracted into a
`build_tool_registry` helper. Tested: store→recall round-trip, type parsing,
and required-content validation.
