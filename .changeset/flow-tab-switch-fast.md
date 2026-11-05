---
'@smooai/smooth': patch
---

SmoothFlow: switching, opening and closing surface tabs no longer rebuilds split views or re-parents terminals. Each tab keeps its own pane tree, Ghostty is only resized when the size really changed, and the tab strip restyles in place. Middle-click closes a tab.
