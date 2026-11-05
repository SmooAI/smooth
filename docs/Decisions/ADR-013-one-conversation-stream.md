---
status: Proposed
date: 2026-10-07
pearl: th-ce7e33
jira: SMOODEV-3709
---

# ADR-013 — One conversation stream: a saved event log, sent to every device

## Status

Proposed (2026-10-07, pearl th-ce7e33, SMOODEV-3709).

## Context

On 2026-10-06 one Big Smooth conversation was driven from both the desktop app
and the iPhone app over Smoo Relay. Over about 2.5 hours it accumulated five
daemon sessions. Three turns died without the user seeing a reply. Desktop and
phone each showed a different version of the conversation. The user's request:
"stream all of the information so both views are the same."

The engine was the smooth-operator `LocalServer`, at rev `9b30ed7b`. It works
like this today:

- **Live events go only to the socket that sent the message.** Turn frames
  (tokens, tool chips, todos, plans, files, errors) go to that connection's
  `sink` (`handler.rs` ~2019). The backplane registers sockets per session and
  per agent, never per conversation. Turns don't publish to it in any case.
- **Only part of each turn is saved.** The user's text and images are saved
  when the turn starts. The assistant's final text is saved only if the turn
  succeeds. Tool calls and results, todos, plans, files, reasoning, usage and
  errors exist only on the live stream.
- **A turn depends on its socket.** Closing the socket aborted the running
  turn (`server.rs` ~881). Backgrounding the phone or dropping the relay
  connection therefore killed turns in the middle of a tool call. The
  SMOODEV-3705 fix stops the abort on disconnect, but the result still reaches
  the device only when it next reloads history.
- **Every bind creates a new session.** Clients mint a fresh session (and the
  phones a fresh `agentId`) on every connect. Nothing lets a device resume a
  stream from where it stopped.

So each device builds its picture from whichever live frames reached it,
topped up by a history that holds less than the live view did. The two views
can't agree.

## Decision

Turn each conversation into **one ordered, saved event log**. Every device
renders from that log, live or replayed, through the same code path.

1. **Event log.** Add a
   `conversation_events(conversation_id, seq, turn_id, origin, ts, type, payload)`
   table. `seq` is a gap-free integer that increases within each conversation.
    - **Event types:**
        - `user_message` (text, images, files)
        - `assistant_text` (streamed deltas, merged into a single item when the
          block ends)
        - `reasoning`, `tool_call`, `tool_result`, `todos`, `plan`, `file`,
          `approval_request`, `approval_resolved`, `usage`
        - `turn_status` (phase, current tool, elapsed time)
        - one terminal event: `turn_done`, `turn_error{code,message}`,
          `turn_cancelled`, `turn_timed_out` or `turn_interrupted`
    - **Saving tokens.** Token deltas go out live to every subscriber, but only
      the merged text is saved. Saved assistant text is checkpointed every few
      seconds, so a crash loses at most that window.
    - **Old message rows.** The existing `message` rows become a derived summary
      for older readers.
2. **Send to the conversation.** The protocol gains a
   `Target::Conversation(id)` backplane target. A socket bound to a
   conversation subscribes to it. The turn runner writes each event to the log
   and then publishes it, so the device that started the turn and every other
   device get the same frames, each tagged with `seq` and `origin`.
3. **Subscribe and resume.** Add
   `subscribe_conversation{conversationId, afterSeq?}`. The server replays
   every event after `afterSeq` from the log, then switches to live. This
   replaces the text-only `get_conversation_messages` as the way clients load
   history. The reply includes the turn in progress, if any (`turn_id`,
   status), so a device that reconnects shows it as running.
4. **Turns belong to the conversation, not the connection.** There is one
   running turn per conversation. A disconnect never stops a turn. Any
   subscriber can send `cancel_turn{conversationId}`. When the daemon starts,
   any turn still open from before a crash is closed with `turn_interrupted`.
5. **Liveness.** While a turn runs, the server sends a `turn_status` event at
   least every 5 seconds. Tools have deadlines (SMOODEV-3705), and the turn as
   a whole has a time limit. Hitting either produces a saved terminal event
   that every device shows.
6. **Stable identity.** There is one session per (device, conversation), and
   it is reused when the device reconnects. Phones keep a stable per-device
   `agentId`.
7. **One shared transcript format.** Desktop (`smooth-web`), iOS and Android
   each build their transcript from the event types above, checked against a
   shared set of JSON test fixtures in `smooth-operator/spec/`. A reload looks
   exactly like the live view, including tool chips, todos, errors and turns
   started on another device ("from iPhone").

## Reasoning

### Identical views come from one source

Two devices show the same thing only if both render the same ordered data.
Patching each client to keep more local state just moves the gap around.
Every device builds its view from the log, so if they hold the same `seq`
they show the same transcript.

### Reconnects are routine

Phones go to the background constantly, and the relay socket was dropping
about every 35 minutes. A resume cursor turns any reconnect into "fetch what I
missed", rather than "start a new session and hope".

### The cost is small

A single-user daemon produces a few hundred events per turn. In SQLite
(`operator-storage.db`) that is a small amount of data, and nothing like the
existing 50 KB tool results. The hosted multi-tenant servers already have
Postgres storage behind the same interface.

## Implementation

- **`smooth-operator` (server and spec):**
    - event log storage trait, with SQLite and Postgres implementations;
    - conversation backplane target;
    - `subscribe_conversation` and `cancel_turn`;
    - `turn_status` heartbeats;
    - JSON Schemas and test fixtures for every event type, implemented across
      all language ports.
- **`smooth` (daemon):**
    - enable the feature in `serve_local_flavor`;
    - relay bridges reuse sessions;
    - desktop `smooth-web` renders from events (drop `renderHistory`'s
      text-only path).
- **`smooai` (`apps/bigsmooth/ios`, `android`):**
    - subscribe with `afterSeq` when the app returns to the foreground or
      reconnects;
    - build the transcript from events.
- **Migration:**
    - On first subscribe, conversations that have no event log get one
      synthesized from the old `message` rows (text and images only).
    - The protocol version is bumped. Old clients keep working through
      `get_conversation_messages`, which is built from the log.

Steps already taken (SMOODEV-3704 to 3708): the compaction fix, turns that
survive a disconnect, tool timeouts, image replay, image-only sends, calendar
output limits, relay grace periods, and client reconnect fixes. These reduce
the damage. They don't make the views identical.

## Consequences

### Positive

- Desktop and phone show the same thing, live and after a reload.
- A turn's outcome (success, error, timeout) is never lost, because it is
  always a saved terminal event.
- Devices can go to the background or switch networks freely.

### Negative

- Every language port gains a protocol surface to keep in sync.
- More writes per turn. Storing token deltas is avoided by merging them before
  saving.
- Event logs need a retention policy (for example, compact finished turns into
  summary events after N days).

### Neutral

- Usage and cost can be read from the log instead of from client-side
  `recordUsage`, so turns run from the phone also appear in desktop Stats.

## Alternatives Considered

### Keep more state on each client and re-fetch history more often

This is cheap, but it never converges. History would still hold less than the
live stream, and a device that wasn't connected at the time misses everything
that happened only live.

### Broadcast live frames to every device without saving them

This fixes "the desktop doesn't see phone turns" while both are connected. It
does nothing for a device that was in the background, which is exactly the
phone's normal state.

## Related

- [[Decisions/ADR-Index]]
- [[Decisions/ADR-007-smoo-relay-remote-control]]
- [[Decisions/ADR-012-daemon-ownership-and-capabilities]]
