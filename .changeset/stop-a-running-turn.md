---
'@smooai/smooth': patch
---

feat(web): Stop button — interrupt a Big Smooth turn that's gone off the rails

There was no way to stop a running turn. Turns are spawned detached in the engine's
WS handler and the canonical protocol had no cancel message, so a user watching Big
Smooth be weird could only send *another* message — which spawned a second concurrent
turn racing the first, producing exactly the contradictory, un-stoppable responses
that motivated this (th-3a912a).

Pins the engine at `e9ce68c` (SmooAI/smooth-operator#332), which adds the `interrupt`
action: per-conversation cancellation registered before the turn is spawned, cancelled
at the turn's next await point. `useOperator` gains `turnActive` + `interrupt()`, and
the composer's primary button becomes a Stop button while a turn is in flight. The
stopped turn closes out with a normal `eventual_response` on its own `requestId`, so
the transcript and `turnActive` unwind through the existing handler — nothing is
cleared optimistically, and the UI can't get out of sync with the daemon.

Enter still sends while a turn runs, so nothing is taken away; the Stop button is
purely additive.
