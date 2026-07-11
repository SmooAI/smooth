---
"@smooai/smooth": patch
---

smooth-agent plugin (0.2.0): ship the shared SmooAI repo guardrail hooks so smooth·smooai·smooblue stop hand-copying `.claude/hooks/`. The plugin now provides `enforce-worktree` (PreToolUse), `session-worktree-warning` (SessionStart), `th-curl-hint` (PreToolUse), and `enforce-pearls-labels` (PostToolUse) — all repo-agnostic (main worktree, parent, and repo name derived from git at runtime), so one source of truth guards every SmooAI repo. Enable per-repo via `enabledPlugins: {"smooth-agent@smooth": true}` and delete the local hook copies.
