---
'@smooai/smooth': minor
---

File transfer, faces:

- **Fix: attached images were silently dropped on th web and th code.** Both sent `images` as bare data-URL strings, but the engine parses `[{ url, detail? }]` objects (UserImage) and fail-soft-discards anything else — so the model never saw the attachment. Now sends `{ url }` objects (matching the mobile apps).
- **SEND: th web renders delivered files as downloads.** Parses the `send_file` directive on `eventual_response` and shows each file as a download link on the assistant message.
