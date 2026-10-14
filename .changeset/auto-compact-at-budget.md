---
'@smooai/smooth': minor
---

The context budget is now each harness's auto-compaction point. `th harness enable` writes `harness.context_budget` (250k) into Claude Code (`autoCompactWindow`) and Codex (`model_auto_compact_token_limit`), so long sessions on 1M-context models compact at the budget instead of near the full window. A threshold you set yourself is left alone. The pearl handoff is still checkpointed before compaction and re-injected after it, and the budget hook now asks for that checkpoint instead of a `/clear`. `harness.context_budgets` adds per-harness and per-model overrides (`claude-code/claude-opus-*=300000`), `harness.context_reset = clear` keeps the `/th-clear` flow, and `th harness budget --report` shows where your Claude Code input tokens went.
