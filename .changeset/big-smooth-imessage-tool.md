---
'@smooai/smooth': patch
---

Big Smooth can read, search and send your macOS Messages (pearl th-1665ed).

A new macOS-only `imessage` tool with five verbs — `recent`, `thread`, `search`,
`conversations` and `send`. Reads go straight at `~/Library/Messages/chat.db`
over an in-process **read-only** SQLite connection (including a decoder for the
`attributedBody` typedstream, so messages composed on modern macOS — which leave
the `text` column NULL — still read). Sending goes through Messages.app via a
fixed AppleScript that takes the recipient and body as `argv`, so nothing the
caller supplies is ever parsed as script.

Both halves run outside the kernel sandbox as a documented trusted-integration
exception (the seatbelt profile denies `~/Library` reads and Apple Events), and
both are still normal tool calls — the permission gate and the Narc hook see them
like any other. `th doctor --setup-imessage` drives the two grants this needs:
Full Disk Access to read, and Automation to send.

Reading exposes the user's message history to the model — a deliberate opt-in,
bounded by per-call row limits (default 20, hard cap 200), a required filter on
`thread`/`search`, body truncation, and attachments reported as a boolean rather
than a filesystem path.
