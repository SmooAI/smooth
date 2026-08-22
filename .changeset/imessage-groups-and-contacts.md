---
'@smooai/smooth': patch
---

Big Smooth can now text a group and name a number.

- **`imessage` group send (th-265003):** `send` takes a `chat` GUID to post to an
  existing group thread, addressed by its exact GUID via `chat id` (the specifier
  that works on current macOS; `text chat id` is broken). It resolves an existing
  chat or fails loudly — it can never invent a phantom 1:1 the way a group _name_
  passed as a handle used to, which is how a message to a group silently vanished.
  `conversations` now returns each thread's send-addressable `chat` GUID, and a
  `send` `contact` that isn't phone- or email-shaped is refused with a pointer to
  `contacts`/`chat` instead of being sent into the void.
- **`contacts` tool (th-ffa500):** a read-only macOS Address Book tool — `lookup`
  a name to phones/emails, or `resolve` a number/email to a name. Turns the bare
  handles `imessage` returns into people, and people into send targets. Read-only,
  so it's available in Plan mode.
