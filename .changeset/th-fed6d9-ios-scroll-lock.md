---
'@smooai/smooth': patch
---

th-fed6d9: Fix the smooth-web chat view scrolling/zooming out of whack on iOS.

The app shell used `min-h-screen` (100vh — ignores the iOS toolbar/keyboard and lets content grow the page), had no viewport zoom lock, and no overscroll guard, so on iOS the chat rubber-banded, pinch/double-tap-zoomed, and the input scrolled off. Now the shell is a fixed `h-dvh` (dynamic viewport, keyboard/toolbar-aware) with `overflow-hidden`; `main` is the scroll container so content pages scroll under a pinned header; the viewport meta locks zoom (`maximum-scale=1, user-scalable=no, viewport-fit=cover`); and `overscroll-behavior: none` + a `height:100%` html/body/#root chain stop the document from bouncing. The chat's existing `h-[calc(100dvh-…)]` sizing now resolves against a real fixed-height shell.
