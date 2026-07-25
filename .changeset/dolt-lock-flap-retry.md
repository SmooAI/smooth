---
'@smooai/smooth': patch
---

Pearl store: unified backoff+jitter retry on the read-only lock-flap
(pearl th-e979ac).

The transient `Error 1105: cannot update manifest: database is read only`
— a concurrent writer (usually another agent's push) briefly holding the
single-writer lock — used to fail fast in CLI mode (auto-doctor found no
orphan → immediate error) and after one respawn in server mode. Now every
dolt write funnels through a single `retry_on_lock_flap` helper that:

1. runs the mode's local self-heal ONCE (CLI: reap an orphaned
   `smooth-dolt serve`; server: force-respawn a wedged child), then
2. backs off with jitter and retries until the op succeeds or a bounded
   budget elapses (`SMOOTH_DOLT_LOCK_RETRY_BUDGET_SECS`, default 30s).

This consolidates the two former one-shot ad-hoc retries into one place
and covers `th pearls` create/update/close, `th msg send`/reply, push,
and memory writes alike — so a momentary collision with a peer's in-flight
push is waited out transparently instead of surfacing. Non-lock errors
(syntax, transport, corruption) still propagate immediately; a genuinely
stuck store returns a clear error with a `th pearls doctor --reap` hint
rather than hanging. The th-mail skill's hand-rolled retry guidance is
dropped since the store now self-heals the flap. No new dependency (jitter
from `SystemTime`).
