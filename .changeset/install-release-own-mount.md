---
'@smooai/smooth': patch
---

install-release.sh no longer leaves a stray SmoothFlow registration behind. Verifying the DMG mounts it, and macOS registers the app on that mount; the script's own cleanup sweep ran before the mount existed, so every run — including `--dry-run` — left exactly the kind of stray copy it exists to remove. It now unregisters its own mount after detaching and on every exit path.
