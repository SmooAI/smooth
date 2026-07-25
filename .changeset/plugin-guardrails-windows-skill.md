---
'@smooai/smooth': patch
---

smooth-agent plugin 0.4.0: pearl-store read-only-wedge guard hook +
windows-build-box skill.

- `pearls-store-guard.sh` (PreToolUse Bash) — nudges away from the three
  patterns that pin the Dolt pearl store read-only for every agent:
  hand-deleting `.smooth/dolt` internals (use `th pearls doctor`), raw
  `dolt`/`smooth-dolt` writes that bypass the single-writer server, and
  backgrounded `th msg watch` / pearl-sync loops. Non-blocking, override
  with `# pearls-guard:ack reason=…`.
- `windows-build-box` skill — self-contained how-to (+ `winrun.sh`) for
  spinning up a throwaway Windows EC2 build box over SSM to iterate on
  Windows builds faster than CI, then tearing it down.
