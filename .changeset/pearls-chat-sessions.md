---
"@smooai/smooth": minor
---

Chat gets sessions. The smooth-web chat page now lists prior
sessions in a sidebar, each persisting its own message history in
the Dolt `session_messages` table. New `/api/chat/sessions`
endpoints (create/list/get/delete + messages). The LLM receives the
last 50 messages on every turn so multi-turn context is preserved.
Session titles auto-rename to the first 60 chars of the opening
prompt. Chat layout fixed so the input row sits flush to the
viewport bottom (no stray scroll).
