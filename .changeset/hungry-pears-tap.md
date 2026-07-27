---
'@smooai/smooth': patch
---

`th code` now remembers the conversation across turns. It built a fresh client per turn and each connect created its own session *and* its own conversation, so Big Smooth started from zero every message — tell it your favorite color and the next turn it answered "you've never told me." The TUI now records the conversation the server binds it to and replays it as `conversationId` on later connects, so every turn appends to one conversation (the same resume path the web SPA uses). This also stops the empty-session churn of one throwaway session per message.
