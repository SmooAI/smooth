---
'@smooai/smooth': minor
---

SmoothFlow learns the state of coding CLIs that have no hooks (th-e77603). Harness manifests gain ordered `[[state.scrape.rules]]`: each rule maps a regex over a chosen part of the pane (`tail`, `pane`, `last_line`, `cursor_line`, `title`) plus optional `all` / `unless` / `unless_below`, output quiet time, recent change, alternate screen and spinner signals to working, idle, needs-you, usage-limit or error, and the first rule that fires decides. Four new built-ins use them — `aider`, `goose`, `crush` and `cline` — each written against real captured panes and driven live through the engine. `prompt_as = "paste"` now waits for the pane to read idle instead of a fixed 4 s, so a first-run question no longer swallows the prompt (th-d2a1e4). A needs-you answered in the pane clears on the next idle, and `resume.mode = "continue_latest"` resumes CLIs that can only continue their most recent conversation.
