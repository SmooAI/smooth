---
'@smooai/smooth': patch
---

Make `Big Smooth.app` distributable (pearl th-a647da): a `make-dmg.sh` that
packages the app into a drag-to-Applications DMG, optional hardened-runtime
signing in `make-app-bundle.sh` (on for a `Developer ID` identity, unchanged for
ad-hoc and the smoo-hub Apple Distribution deploys), a `notarize-and-staple.sh`
that no-ops cleanly without Apple credentials, and a `macos-app.yml` release job
that builds/packages/notarizes and attaches the DMG to the GitHub Release.

The app now bundles the `th` CLI at `Contents/Resources/bin/th`, and the menu bar
gained **Install th CLI…** — it symlinks the bundled binary onto `PATH`
(`/usr/local/bin`, falling back to `~/.local/bin`), the VS Code "install shell
command" pattern.
