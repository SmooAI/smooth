---
'@smooai/smooth': patch
---

fix(web, th code): block send while a turn is in flight — no more concurrent turns

Sending a message while a turn was already running did **not** queue behind it: the
daemon spawned a second concurrent turn, the two streamed back interleaved, and each
answer landed under the other's prompt. The agentic conversation bench reproduces it
with plainly swapped responses, and it's the root cause behind the contradictory
date/calendar replies (th-426791). The Stop button (th-3a912a) gave a way out of a
bad turn but left the footgun itself in place.

Send is now blocked outright while a turn is in flight, in both composers:

- **smooth-web** — Enter and the send button both go through one pure predicate
  (`canSend` in `turn-guard.ts`). The draft stays in the box, the placeholder reads
  "Big Smooth is working — Stop to interrupt", and a line under the composer explains
  the paused Enter once you've typed something. Stop remains the way through.
- **`th code`** — Enter refuses to dispatch a second agent turn before it takes the
  input, so nothing is lost. Slash commands and `!shell` are local and stay live, and
  the input box titles itself "Working… send paused" so the swallowed keystroke reads
  as intent rather than a bug.

Blocking rather than queueing: a queued message would be composed against a
conversation state the user never saw, and silently firing it later is the same
surprise the concurrency bug produced. Stop is an explicit, visible alternative.
