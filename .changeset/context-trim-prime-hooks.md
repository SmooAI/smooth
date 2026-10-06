---
'@smooai/smooth': minor
---

Cut the context the smooth-agent plugin and `th prime` inject into every session.

- `th prime` lists at most 10 ready pearls (`--ready-limit N` / `$TH_PRIME_READY_LIMIT`), ending with `… +N more`, and the workflow primer is a terse rules block instead of a long command reference.
- The compact/resume handoff hook only injects pearls this session is actually on: in the primary checkout, the ones it checkpointed or whose id is in the branch name (it used to inject every pearl ever checkpointed there). At most 3 packets, each capped. PreCompact applies the same filter, so it no longer stamps every pearl with the session id.
- `mail-guard.sh stop` no longer blocks a session just because no mail watcher is armed; it blocks only for unread direct mail, once per distinct count. The unarmed reminder is one line of prompt context, once per session.
- Broadcast `note`/`result` messages no longer wake the th-mail watcher or count toward the unread hint: new `th msg watch --wake` and `th msg unread-count --wake` (direct mail plus broadcast request/handoff/cancel).
- SessionStart registration prints one line.
