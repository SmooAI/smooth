---
'@smooai/smooth': patch
---

Big Smooth: the `calendar` tool returns a compact, bounded event list (`limit`, default 25, with a "N more events" note) instead of byte-truncated raw `ical` JSON, and `search` defaults to a short window; a phone's relay chat bridge now survives `peer_offline` or a relay socket drop for a 10-minute grace window so the running turn isn't killed; the desktop chat re-binds and reloads the active conversation after a reconnect instead of silently starting a new one, refreshes the sidebar when history reloads, and sends image-only messages with an `(image attached)` placeholder.
