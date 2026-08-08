---
'@smooai/smooth': patch
---

Fix macOS Calendar/Reminders (EventKit) grants on the Electron Big Smooth app.

macOS only shows the EventKit prompt for a signed bundle whose main executable
asks, and the Electron app's daemon runs as a child — so `Set Up → Calendar…`
never prompted. Ship a signed nested helper bundle
(`Big Smooth.app/Contents/Helpers/BigSmoothTCC.app`) whose main executable is
`smooth-daemon`, assembled + signed by an electron-builder `afterPack` hook.
`grantEventKit()` and `th doctor --setup-calendar`/`--setup-reminders` now launch
the helper via `open --args tcc <what>` (which prompts) and poll the resulting
grant status.
