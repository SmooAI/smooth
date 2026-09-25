---
'@smooai/smooth': patch
---

SmoothFlow tabs, ⌘W and the default directory (th-96fcb7):

- **⌘W on the last pane no longer closes the window.** It empties the view, and the session keeps running in the fleet.
- **Tabs have real titles:** the pearl, else the session title, else the folder name. They no longer read `/`.
- **New sessions default to your home folder, not `/`.** An app launched from Finder or the Dock inherits `/` as its working directory, and the daemon used it as the workspace. A daemon whose working directory is `/` now uses `$HOME`, and SmoothFlow starts its daemon in `$HOME`.
