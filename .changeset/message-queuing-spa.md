---
'@smooai/smooth': patch
---

Big Smooth SPA: queue messages typed while a turn is in flight. Instead of blocking the send (the old `turn-guard`), a message typed mid-turn is held in a client-side queue and sent automatically — one at a time, in order — when the active turn's reply lands. Queued messages render as removable chips above the composer with a "Clear queued" affordance that empties the queue without interrupting the running turn (distinct from Stop, which cancels only the turn). Idle sends are unchanged.
