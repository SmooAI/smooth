---
'@smooai/smooth': minor
---

th-3be564: Big Smooth chat is now multimodal — attach images to a chat message and the assistant sees them.

**Web** (`smooth-web` chat): a paperclip button, clipboard paste, and drag-and-drop stage image/PDF attachments; the composer shows removable thumbnails and the sent user bubble renders the images inline. Attachments ride the existing `POST /api/chat/sessions/{id}/messages` as `{ content, attachments: [{ name, mime, data(base64) }] }` — the field is optional, so text-only sends are unchanged.

**Daemon** (`smooth-bigsmooth`): the chat routes accept `attachments`, content-address the bytes under `~/.smooth/attachments/`, and build a multimodal turn via the engine's `Message::user_with_images`. When a turn carries ≥1 image the model auto-routes to `gemini-2.5-flash-lite` (a proven cheap vision model that also reads PDFs natively); text-only turns keep the default coding-slot model. An explicit per-request `model` still wins. Documents are marker-only this phase (full ingest is the follow-up). Consumes the engine's new multimodal message/wire model via a git-rev pin bump (smooth-operator-core PRs #58/#62).
