---
'@smooai/smooth': minor
---

Big Smooth: bump the agent engine to core `1.14.1` and the smooth-operator server to `19da0e9`. Turns now survive a client disconnect, image-only sends work, history replays images, failed turns are recorded in the conversation, and a conversation runs one turn at a time. The web app shows a "turn still running" banner with a Stop button when a send is refused with `TURN_IN_PROGRESS`, and renders recorded turn failures as error cards.
