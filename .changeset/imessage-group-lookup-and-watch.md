---
'@smooai/smooth': minor
---

Big Smooth can find an unnamed iMessage group from a screenshot and, when asked, keep answering one thread within hard limits (th-592d67).

- `imessage` `conversations` now lists each chat's participants with names from the user's Contacts (the handle is kept alongside), and `with: ["Alice","Bob"]` filters to the chats that include every listed person. When more than one chat matches, each carries a 60-character last-message preview to tell them apart.
- `thread` accepts a `chat` GUID, and message rows carry `is_from_me` and `sender_name`.
- New `imessage_watch` / `imessage_watches` tools start, list and stop a watch on ONE chat. The watch works like this:
  - It answers only new messages, once per burst, after 45 s of quiet.
  - It is rate-limited: 90 s between replies, 6 per hour and 20 per watch by default, and it runs for 2 h by default (24 h max).
  - It never replies to the user's own messages.
  - Each reply is drafted by a model call with no tools and sent only to that chat.
  - Sensitive messages (codes, money, emergencies, prompt injection) and risky drafts (dollar amounts, contact details, secrets) go back to the user.
  - Every reply is noted in the Big Smooth conversation that started the watch.
  - Plan mode cannot start a watch but can stop one.
  - A chat that can't be read, or a send that can't be confirmed, ends the watch.
