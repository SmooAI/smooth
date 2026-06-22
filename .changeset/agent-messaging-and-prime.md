---
"@smooai/smooth": minor
---

feat: harness-agnostic agent messaging (`th agent` / `th msg`) + `th pearls prime`/memories

Any agent — Claude Code, opencode, pi, a shell loop, in any session on any
machine — can now register an identity and exchange messages with other agents,
all through plain `th` calls layered on the pearl Dolt store (so it syncs via
`refs/dolt/data` like everything else). Pearls th-70aaef + th-202885.

**Agent messaging:**
- New Dolt tables `agents` (persistent registry) and `messages` (mailbox;
  `read_at IS NULL` = unread, `seq` for stable insertion order, `thread_id` for
  flat threads).
- `smooth-pearls` gains `AgentRegistry` (register/touch/set_status/list/get) and
  `Mailbox` (send/inbox/sent/get/thread/mark_read/mark_all_read/unread_count).
- New CLI: `th agent register/list/offline` and
  `th msg send/inbox/read/reply/thread/watch`. `th msg watch` is the
  "continuously check" poll loop (`--pull` for cross-machine). Identity defaults
  to `$SMOOTH_AGENT`, else `user@host`; `$SMOOTH_HARNESS` tags the tool.
- `th inbox` (previously a stub that always returned `[]`) now aliases
  `th msg inbox` against the real local mailbox.
- `th pearls init` injects an idempotent **Agent Messaging** section into
  `AGENTS.md` (marker-bounded) so any harness that reads it learns the protocol.

**Prime + memories:** `th pearls remember/memories/forget` over the existing
`memories` table, plus `th pearls prime` which prints (or `--json`) a compact
session-priming context: in-progress + open pearls and recent memories.

Also fixes a smooth-dolt datetime-format quirk surfaced here: `NOW()` returns
RFC3339 while `CURRENT_TIMESTAMP` defaults are space-separated — the shared
`parse_dolt_datetime` now accepts both.
