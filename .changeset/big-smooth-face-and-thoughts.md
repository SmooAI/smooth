---
"@smooai/smooth": minor
---

Big Smooth chat UI: three.js animated face + live thought stream

- Add a mesh-based face for Big Smooth in the chat header that uses the
  th-in-Smooth logo gradient (teal → blue), bobs and rotates calmly when
  idle, and switches to a faster scan + brighter glow when streaming.
- Stream live "thoughts" via the Fast slot (Gemini Flash Lite) — every
  tool call and intermediate assistant turn is summarized into one
  short, first-person sentence and broadcast over the chat WebSocket
  as `BigSmoothThought`. The chat page surfaces the most recent three
  as floating bubbles next to the face, with the static "Big Smooth is
  thinking…" line removed (the face + bubbles convey it).
- Rate-limited (Semaphore-capped at 2 in-flight) and non-blocking —
  the agent loop never waits on the summarizer.
