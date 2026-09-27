---
'@smooai/smooth': minor
---

A `smoothflow` skill (th-1efb59) teaches an agent to run other agents through SmoothFlow. It covers starting one in a repo or pearl worktree, prompting it and waiting for the turn, relaying approvals to the user rather than answering them itself, fanning out, and closing out. It uses the `flow_*` MCP tools or `th flow`. The smooth-agent plugin installs it, and `th flow skill` prints it for any other harness. A test fails if a flow MCP tool ships without the skill naming it.
