---
'@smooai/smooth': patch
---

SmoothFlow macOS: the Settings window uses the classic grouped tab strip. Under the macOS 26 SDK a plain `TabView` became a window-toolbar tab bar, and in the titled Settings window every tab collapsed into a `»` overflow — the CI runner (Xcode 26.6) showed an empty toolbar and the Daemon / Phones panes were unreachable, which had the `smoothflow-mac` lane red on every PR since the Phones pane landed (th-2ecc1c).
