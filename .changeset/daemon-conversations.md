---
'@smooai/smooth': minor
---

th code sessions ARE daemon conversations (pearl th-aaa53a, epic th-d7366d). The Ctrl+B sidebar now lists Big Smooth's `list_conversations` — the same rows the web SPA shows — so a chat started in any face is resumable from every face; resuming binds the conversation and hydrates the transcript from stored history over the canonical protocol. Local `~/.smooth/coding-sessions/` JSON is no longer written (legacy sessions remain readable as the offline fallback). The client-side `prior_messages` replay is gone — the engine already replays a resumed conversation's history by thread_id, so the TUI was double-feeding context. Also fixes "New conversation" not unbinding the daemon conversation, which made the next turn silently resume the chat the user just left.
