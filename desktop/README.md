# Big Smooth desktop (MVP)

An Electron shell around the native `smooth-daemon`. The daemon stays the engine —
this is the window and the tray that replace the browser PWA and the macOS-native
menu bar (`SMOOTH_MENUBAR`).

## Run it

```bash
cd desktop
pnpm install
pnpm dev        # tsc && electron .
```

Or from the repo root: `pnpm dev:desktop`.

## What it does

- **Daemon lifecycle.** On launch it probes `http://127.0.0.1:8787/health` (or
  `$SMOOTH_ADDR`). If a daemon already answers — `th up`, a launchd unit — it
  attaches to it and never touches it. Otherwise it spawns `smooth-daemon run`
  and terminates that child on Quit.
- **Window.** A `BrowserWindow` on the daemon's `/`. The daemon serves smooth-web
  with its local auth token already injected into `index.html`, so there is no
  renderer, preload, or IPC code here. Closing the window hides it; Quit exits.
- **Tray.** The `th` mark, with Open / Set Up (Calendar, Reminders, Messages, Full
  Disk Access — each opens a terminal running the matching `th doctor` flow) / Quit.

`smooth-daemon` resolution order matches `th daemon`: bundled resources →
`$SMOOTH_DAEMON_BIN` → `~/.smooth/bin` → `PATH` → the cargo target dir. In dev,
`pnpm install:th` already puts it on `PATH`.

## Not done yet

Packaging and signing. `electron-builder.yml` is a working skeleton (mac/win/linux
targets, `extraResources` pointing at a per-platform `resources/<os>/smooth-daemon`
that nothing populates yet) but no certificates, notarization, or CI job. `pnpm dist`
produces unsigned local artifacts. Only the macOS run has been exercised.
