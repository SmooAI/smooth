---
'smooai-smooth-daemon': minor
---

Fix the `SMOOTH_AGENT_MODEL` override (it was a no-op) and add `SMOOTH_FAST_MODE`
— a fast mode that points Big Smooth at `groq-gpt-oss-120b` (current, fast,
strong tool-caller, reasons on the harmony channel so thinking shows cleanly).
Model priority: `SMOOTH_AGENT_MODEL` > fast-mode pin > coding route.
