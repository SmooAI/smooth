---
'@smooai/smooth': patch
---

th-mail: broadcasts reach only agents registered before they were sent and expire after 72 hours, and the Stop hook no longer blocks on broadcasts. Every new session registers a fresh handle, so it used to inherit the machine's whole broadcast history and could not stop until it had read and acked all of it. Adds `th msg unread-count --direct` (direct mail only).
