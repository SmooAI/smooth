---
'@smooai/smooth': patch
---

Big Smooth desktop updates install when you click Install Update (th-6b5d5c). `quitAndInstall()` closes every window before Electron fires `before-quit`, but the window's close-to-tray handler only let a close through once `before-quit` had run, so it cancelled those closes. The app never exited, and Squirrel's installer gave up with "App Still Running Error" (it also made 0.1.13's "Restart now" look dead for minutes). The updater now marks the exit before calling `quitAndInstall()`, and if the app is somehow still alive 15 seconds later it exits so the installer can swap the bundle. This takes effect for updates installed FROM this version onward; a copy older than this still needs Quit from the menu bar to finish its update.
