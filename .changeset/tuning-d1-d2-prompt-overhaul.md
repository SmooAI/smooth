---
"@smooai/smooth": patch
---

Adopt Claude Code v2.1.120 + opencode tuning patterns in the operator runner and Big Smooth chat-tools prompts

Operator runner system prompt (`crates/smooth-operator-runner/prompts/system.md`)
fully rewritten around five high-leverage discipline blocks lifted from the
Claude Code v2.1.120 prompt and opencode's `anthropic.txt`:

- Restraint: no premature abstraction (three similar lines beats one), no
  validation for can't-happen scenarios, no comments by default — only WHY
  when non-obvious, never WHAT or task references that rot.
- Verify before claiming done: type-check + tests must pass; "code correctness
  is not feature correctness; if you can't exercise the feature, say so."
- Blast radius / reversibility: explicit destructive-op list (rm -rf, git
  reset --hard, force push, package downgrade, CI/CD edits, sending messages)
  each requiring scope-bounded authorization. "Authorization stands for the
  scope specified, not beyond."
- Communication discipline: one sentence before first tool call, short
  updates at find/pivot/blocker, two-sentence end-of-turn summary, no colons
  before tool calls.
- Loop hygiene: don't retry failing commands in sleep loops; diagnose root
  cause; don't repeat a rejected call.

Existing Smooth-specific operational guidance (project_inspect, lsp,
edit_file/write_file/apply_patch, bg_run, mise) preserved and trimmed.

Big Smooth chat-tools prompt (`crates/smooth-bigsmooth/src/chat_tools_system_prompt.txt`)
gets three additions on the same theme:

- `teammate_spawn` rule 3 now requires a `context_brief` — "brief the teammate
  like a smart colleague who just walked into the room: what you've learned,
  what you've ruled out, files to look at, judgment-call dimensions to flag.
  Never delegate understanding."
- New "trust but verify" line on the workflow: spot-check teammate output by
  reading a file or running the build before reporting work as done.
- Style block extended with the same one-sentence-before / no-colon /
  exploratory-question discipline rules.

Both files compile via `include_str!` with no Rust changes needed; the
operator-runner binary will pick up the new prompt on next rebuild via
`scripts/build-operator-runner.sh`.
