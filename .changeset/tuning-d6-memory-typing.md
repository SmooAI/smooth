---
"@smooai/smooth": patch
---

D6: intent-based memory typing + verify-before-recommend rule on recall

Adds four intent-based variants to `MemoryType` adapted from the Claude
Code v2.1.120 memory subsystem:

- `User` — durable facts about the user (role, expertise, preferences).
- `Feedback` — corrections or validations on approach. Highest leverage
  type — re-reading prevents re-litigating decisions.
- `Project` — current state of in-flight work (initiatives, deadlines,
  who's doing what). Decays fast.
- `Reference` — pointers to where information lives outside this
  project (Linear, Slack, Grafana, etc.).

The original scope-based variants (`ShortTerm`, `LongTerm`, `Entity`)
are preserved unchanged for backward compatibility.

`MemoryType::needs_freshness_check()` flags `Project` and `Reference`
as time-sensitive. The agent-context builder
(`Agent::build_context_messages`) checks recalled entries and, when any
need a freshness check, prepends a verify-before-recommend note to the
recalled-memories block:

> Note: 'the memory says X exists' is not the same as 'X exists now'.
> Before recommending or acting on any function path, file, flag, or
> external pointer named below, verify it's current by reading the file
> or grepping the codebase. Project and Reference memories are
> time-sensitive; User and Feedback are durable.

This is the structural counterpart to the recall-discipline auto-memory
rules: now the runtime nudges the model to verify, not just the prompt.

2 unit tests (variant serialization round-trip; freshness-check guard).
