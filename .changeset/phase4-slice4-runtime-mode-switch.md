---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Phase 4 Slice 4 (th-bd0def): runtime permission-mode switching. The
daemon's Gate-1 posture is now a thread-safe `SharedPermissionMode`
(atomic-backed) instead of a fixed start-time value, and a new
`PUT /api/mode` endpoint switches it live (400 on an unknown mode);
the change takes effect on the next dispatched task. The control surface
turns the header permission-mode badge into a dropdown that switches
posture via the endpoint and re-reads `/api/status`.
