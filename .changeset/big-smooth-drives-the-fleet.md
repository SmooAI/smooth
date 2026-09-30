---
'@smooai/smooth': minor
---

Big Smooth drives the SmoothFlow fleet (th-8b3918). The daemon's in-process agent now has the `flow_*` tools — `flow_list`, `flow_snapshot`, `flow_handoff`, `flow_harnesses`, `flow_repos`, `flow_infer`, `flow_new`, `flow_send`, `flow_prompt_wait`, `flow_approve`, `flow_kill`, `flow_close`, `flow_fanout_new`, `flow_fanout_pick` — called straight on the daemon's flow engine, with the same names, arguments and answers as the `th mcp serve` tools. A new `project_setup` tool takes a repo path or a git URL (cloned under `~/dev`), makes a worktree for a pearl or branch, and starts a coding agent there in one step. Every tool that changes the fleet waits for the user's confirmation, whatever the auto-mode setting. `flow_approve` always does. In Plan mode only the read tools remain, and sidekicks never receive a tool that needs confirmation.
