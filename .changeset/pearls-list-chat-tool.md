---
"@smooai/smooth": patch
---

Add `pearls_list` chat tool — fixes deadlock when asking pearl-count questions

The chat agent's `bash` tool would gladly run `th pearls list` to answer
"how many open pearls do I have", but `th` re-enters Big Smooth's own
dolt store via a fresh CLI subprocess, which deadlocks against the
long-running `smooth-dolt serve` companion. The chat hung indefinitely.

Fix:
- New `pearls_list(status?, limit?)` chat tool that calls
  `state.pearl_store.list(...)` directly through the existing
  serve-backed handle. Answers in milliseconds.
- `bash` tool gains an explicit forbid-list (`th`, `smooth-dolt`,
  interactive editors) so the model can't accidentally re-trigger the
  deadlock. Surfaces a clear error pointing the agent at the native
  pearl tools.
- `bash` timeout tightened from 25 s → 10 s. Slow commands belong in
  a teammate, not blocking the chat agent.
- System prompt explicitly steers pearl questions to the native
  tools.

Verified: "how many open pearls do I have right now?" went from an
infinite hang to a 4.0 s round-trip.
