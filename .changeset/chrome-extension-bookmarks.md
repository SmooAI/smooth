---
'@smooai/smooth': minor
---

`th chrome`: the Smooth Chrome extension plus a native-messaging host let `th` and your agents read and write Chrome bookmarks, including account-synced ones, through `chrome.bookmarks`, so Chrome Sync carries changes to your other devices. Commands: `th chrome install`, `th chrome bm tree|search|upsert|mkdir|add|move|rename|rm|export`. Every change is committed to a git history with `restore <rev>` and optional push to a remote. Turn on browsing in the extension popup and agents can also list, open, read, click, type in and screenshot tabs. Every command is also an MCP tool (`bookmarks_*`, `browse_*`).
