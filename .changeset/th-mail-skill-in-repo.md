---
'@smooai/smooth': patch
---

Bring the `/th-mail` Claude skill into the repo (git-tracked) + a symlink installer.

- Adds `.claude/skills/th-mail/` (SKILL.md + watch-once.sh) — the harness-agnostic agent-mail watcher (`watch-once.sh` blocks until unread `th msg` mail arrives, prints it, and exits so a background task re-invokes the agent; no busy-poll; does NOT `--pull` by default to avoid the Dolt write-lock contention that caused store-wide `Error 1105: database is read only`).
- Adds `scripts/install-skills.sh` + `pnpm install:skills`, which symlinks the repo's skills into `~/.claude/skills` (backing up any existing copy). The skill now lives in ONE git-tracked place, so it can't be silently changed by an untracked local edit. Output follows the Smooth Flow glyph vocabulary.
