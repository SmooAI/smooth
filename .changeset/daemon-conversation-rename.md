---
"@smooai/smooth": patch
---

feat: rename conversations from the Big Smooth daemon PWA sidebar

Each sidebar row gains a rename affordance (a hover pencil icon, or double-click
the row) that opens an inline text input in place of the title. Enter commits,
Esc (or blur) cancels. The rename is applied optimistically to the row, then the
server's canonical (sanitized) title is reconciled onto it from the
`rename_conversation` reply.

Wires a new `renameConversation(id, title)` into the `useOperator` hook, which
sends the server's `rename_conversation` WS action
(`{action, requestId, conversationId, title}`) and reconciles the echoed
`{conversationId, title}`. Pairs with the server-side auto-title + rename
(pearl th-d5b446): auto-generated titles now appear in the sidebar, and users can
override them.
