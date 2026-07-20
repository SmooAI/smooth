---
'@smooai/smooth': patch
---

Big Smooth web: lock the mobile viewport. On iPhone the PWA could be pinched
and panned so the UI drifted off-screen. `index.html` already set
`user-scalable=no` and `maximum-scale=1`, but iOS Safari has ignored both since
iOS 10, so the meta tag was doing nothing.

The shell is now genuinely fixed: `html`/`body`/`#root` get `position: fixed`,
`100dvh`, `overflow: hidden` and `overscroll-behavior: none` (the latter alone
only stops scroll *chaining*, not page panning), plus
`touch-action: manipulation` to drop double-tap zoom. Pinch-zoom is cancelled
in `main.tsx` via WebKit's non-standard `gesture*` events — the only thing that
blocks it on iOS. Only inner panes scroll.
