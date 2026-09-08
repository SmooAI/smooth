---
'@smooai/smooth': patch
---

th-1fca98: render images another client attached, and bump the engine to persist them.

A photo sent from the iOS app showed only as text in the desktop app's view of the same conversation. The engine now persists a user turn's images as `image` content items (smooth-operator #564), and `smooth-web` renders them from history: history parsing moved to a pure, unit-tested `history.ts` that turns persisted `image` items into renderable attachments (the composer's live-send path already showed them).

Engine bump: `smooth-operator-server`/`svc` git rev `b6c6b84` → `9b30ed7b` (includes the image-persistence fix), which moves core `1.7.10` → `1.10.0`. That core carries three additive `AgentEvent::Completed` fields (spend taint flags + response id, th-126fe6) and makes `Session.agent_id` optional; the few construction sites were updated to the serde defaults older output already produced (no behavior change).
