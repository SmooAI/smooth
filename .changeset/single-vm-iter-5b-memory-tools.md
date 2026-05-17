---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 3 iter-5b. Agent-callable tools backed
by the iter-5a `MemoryStore`. Lets the agent decide when to
write learned-context notes and read them back without
touching the dispatch path.

Three tools registered via
`smooth_pearls::register_memory_tools(registry, store)`:

* `remember(content, source?)` — append a note. `source`
  defaults to `"manual"`; agents typically tag with their
  current pearl id.
* `recall_recent(limit?)` — newest-first list; default 20,
  clamped to [1, 100]. Returns "no remembered notes yet"
  when empty.
* `recall_by_source(source, limit?)` — filter to a specific
  origin (a pearl id, an operator id). Useful for "pick up
  where I left off on `th-abc123`".

Read-only flags are wired so callers that gate writes can
distinguish — `recall_*` are read-only; `remember` is not.

Tool descriptions emphasize concrete short notes — the agent
should remember facts, gotchas, commands, paths, not full
sentences of narrative. The system prompt is what teaches
the agent when to invoke these (top of task = `recall_recent`,
end of task = `remember`).

7 new tests cover the happy round-trip, empty-store friendly
message, source-filter behavior, default-source fallback,
missing-content error, limit clamping at both ends, and the
read-only-flag advertisements.

iter-5b is the last Phase 3 deliverable. The agent-side
system-prompt nudges to actually call these can land
incrementally without another iter — that's a prompt edit,
not architecture.
