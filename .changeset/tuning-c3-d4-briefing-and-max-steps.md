---
"@smooai/smooth": patch
---

Enforce `context_brief` on `teammate_spawn` and inject a max-steps reminder
on the agent loop's final iteration

### C3 — `context_brief` is now a structurally-required field

`crates/smooth-bigsmooth/src/chat_tools.rs` `TeammateSpawnTool`:
- Adds `context_brief` as a required tool-schema field with `minLength: 80`.
- `execute()` rejects any call where the trimmed brief is under 80 chars
  with a teaching error message that lists what a real briefing covers
  (what you've learned, what you've ruled out, files/paths/commands to
  start with, judgment dimensions to flag back) and tells the model to
  re-issue rather than just retry.
- The teammate's task message is now structured: pearl description →
  `## Context from team lead` → context_brief → optional
  `## Extra constraints` → extra_prompt. Teammates get a clear scaffold
  instead of one big concatenated blob.

This is the structural enforcement of the prompt-side rule landed in the
D2 batch: previously the chat agent could ignore the rule; now the
runner rejects the call and forces a recovery turn.

### D4 — max-steps reminder on final iteration

`crates/smooth-operator/src/agent.rs`:
- New `MAX_STEPS_REMINDER` constant (adapted from opencode's
  `max-steps.txt` — opencode's tool-disabling reminder works because
  it instructs a clean wrap-up rather than reading like an error).
- Both `run()` and `run_with_channel()` push the reminder as a system
  message on the final iteration before the LLM call. The model sees
  "this is your final iteration; respond with text only — what's done,
  what's left, what to recommend next" and writes a useful summary turn
  instead of starting a tool chain that gets cut off.

### Tests
- 3 new chat_tools unit tests (threshold range, rejection-message scaffold,
  schema-name stability)
- 1 new agent test (`max_steps_reminder_includes_recovery_scaffold`)
- All existing tests still pass

Together with D1+D2 (prompt rewrites) this completes the prompt-and-loop
half of the typed-sniffing-badger Pillar D plan. C1 (per-role tool
clearance enforcement at the runtime layer) and C4 (trust-but-verify
hook) remain for follow-up work.
