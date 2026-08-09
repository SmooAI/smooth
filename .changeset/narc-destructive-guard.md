---
'@smooai/smooth': patch
---

Narc now judges destructive actions in any tool, not just shell commands.

`detect_dangerous_cli` only ever inspected shell commands, so destruction done
through a structured tool — a `write_file` that empties a data file, a delete
tool, a `calendar delete` — reached no detector and never escalated to the LLM
judge. A new detector keys on the effect (data that exists stops existing) and
routes those calls through a purpose-built judge prompt, failing closed.
