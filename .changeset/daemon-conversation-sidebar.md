---
"@smooai/smooth": minor
---

feat: conversation sidebar + resume + new-chat in the Big Smooth daemon PWA

The smooth-web SPA (`crates/smooth-web/web`) was a single, ephemeral chat — one
fresh session per load, no history. It now has a toggleable slide-in sidebar
that lists your recent conversations and lets you jump between them. Pearl
th-d5b446.

- **operator.ts** speaks three more actions of the smooth-operator WS protocol:
  `list_conversations` (populates `conversations` for the sidebar, refreshed
  after every turn), `create_conversation_session` with a `conversationId` (to
  **resume**), and `get_conversation_messages` (loads a resumed conversation's
  history into `messages`). New hook surface: `conversations`,
  `activeConversationId`, `refreshConversations()`, `resumeConversation(id)`,
  `newConversation()`.
- **App.tsx** gains a menu-toggle + Aurora-Glass `Sidebar`: overlays with a
  backdrop on mobile, docks on desktop, highlights the active conversation, and
  carries a prominent "New chat" button. The Smooth brand mark moved into the
  sidebar header. No sidebar interaction leaves the current single-chat
  behaviour unchanged.

Back-compat: history is rendered as text-only (past tool chips aren't
reconstructed), and the message/`updatedAt` shapes are read defensively pending
final reconciliation with the server-side action.
