---
"@smooai/smooth": minor
---

runner: add `todo_list` tool for cross-turn task state (opencode parity)

Adds a `todo_list` tool to smooth-operator-runner. Operates on a small
JSON file at `.smooth/todos.json` with four actions:
`add` / `list` / `update` / `clear`. Persists across the runner's
fresh-per-turn process boundary so on turn 2 the agent can
`todo_list action='list'` to find what it was doing — the structural
anchor opencode uses and smooth was missing.

Pearl `th-1d6699`. Diagnosed by side-by-side pane capture of opencode
vs smooth on `cleanup-node-modules-orphans`: opencode emits a
`# Todos` checkbox list as part of its plan, marks items in_progress
as it executes, and on `"yes, proceed"` reads the pending todo and
issues ONE concrete `rm -rf <paths>` command. Smooth had no equivalent
tool — every other registered tool (read_file, write_file, edit_file,
apply_patch, list_files, grep, lsp, bash, bg_run, http_fetch,
project_inspect, read_memory, write_memory) is single-shot or
project-scoped, none track per-task state.

Wired through:
- `crates/smooth-operator-runner/src/main.rs` — `TodoListTool` impl
  + `TodoStore` (JSON-file-backed, atomic rename-from-tmp write) +
  8 unit tests including cross-process persistence.
- `crates/smooth-bigsmooth/src/policy.rs` — added `todo_list` to both
  `registered_tool_names()` and `read_only_tool_names()`. Without
  this entry Wonk denies every call and the agent logs the
  "I cannot use the todo_list tool" excuse.
- `crates/smooth-operator/src/cast/prompts/fixer.txt` — new section
  teaching the agent the planning → executing → completion lifecycle
  for the tool. Anchored on "call `list` at the start of every
  continuation turn — it tells you what was already done and what's
  next."

Bench impact at `deepseek-v4-flash`: not measurable — the weak model
hallucinates "tool not in allowlist" rather than calling it (no
allowlist gate exists in direct mode; the LLM is making up an
excuse). The tool is structurally in place for stronger models
(v4-pro, claude-sonnet) where the multi-turn discipline pays off.
Filed as architectural parity, not a single-fixture lift.
