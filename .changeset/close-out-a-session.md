---
'@smooai/smooth': minor
---

SmoothFlow: close out a session from anywhere, not just a finished Inbox card. The sidebar row's context menu and **Session ▸ Close Out…** (⌘⌥W) both send `flow.close`, and a _running_ session closes the same way — the engine kills it first and the sheet says so before you confirm. The sheet now shows what it will destroy, not only what it will do: branch, uncommitted-file count and the pearl's title. Merged state stays uncached on purpose — the engine reveals it by refusing, and force is still offered only after that reason has been read. A refusal started outside the Inbox gets its own sheet instead of vanishing into the rail.
