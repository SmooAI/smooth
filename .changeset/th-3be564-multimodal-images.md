---
'@smooai/smooth': minor
---

th-3be564: Big Smooth chat is now multimodal — attach images to a chat message and the assistant sees them.

**Web** (`smooth-web` chat): a paperclip button, clipboard paste, and drag-and-drop stage image/PDF attachments; the composer shows removable thumbnails and the sent user bubble renders the images inline. Attachments ride the existing `POST /api/chat/sessions/{id}/messages` as `{ content, attachments: [{ name, mime, data(base64) }] }` — the field is optional, so text-only sends are unchanged.

**Daemon** (`smooth-bigsmooth`): the chat routes accept `attachments`, content-address the bytes under `~/.smooth/attachments/`, and build a multimodal turn via the engine's `Message::user_with_images`. **Images and PDFs** both ride the turn as `data:` media parts and auto-route to `gemini-2.5-flash-lite` — verified live that the gateway reads a PDF sent this way natively (it answered a question about the file's contents), so "read this PDF" works with no separate ingest pipeline. Text-only turns keep the default coding-slot model; an explicit per-request `model` still wins. Other document types (docx/xlsx/…) are marker-only until a converter lands (a docling microVM is the planned upgrade). Consumes the engine's new multimodal message/wire model via a git-rev pin bump (smooth-operator-core PRs #58/#62).
