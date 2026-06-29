---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Reasoning models now show their thinking as a quiet, collapsible "thought for a
moment" aside instead of bleeding chain-of-thought into the answer (th-4d8682).
smooth-operator-core emits reasoning on a distinct `AgentEvent::ReasoningDelta`,
operator-server maps it to a `stream_reasoning` protocol message, and smooth-web
captures it into a separate field rendered as a collapsed disclosure — the answer
(`stream_token`) stays clean. Fixes the daemon's visible chain-of-thought with no
model change.
