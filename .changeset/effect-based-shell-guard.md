---
'@smooai/smooth': patch
---

Destructive shell commands are now caught by effect, not by pattern.

Narc snapshots the workspace's small files before a shell call and, after it
runs, restores anything that was deleted or emptied — then reports it, so the
agent asks for confirmation instead of succeeding over destroyed data. This
covers spellings no pattern list can enumerate (`tee`, `sed -i`, `python3 -c`),
which the agentic bench found empirically.
