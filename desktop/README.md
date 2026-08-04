# Big Smooth desktop

The Electron app that IS Big Smooth: one installable, cross-platform. It bundles
the native `smooth-daemon` as its engine, opens a window on the daemon's web UI,
and owns the tray.

## Run it

```bash
cd desktop
pnpm install
pnpm dev        # tsc && electron .
```

Or from the repo root: `pnpm dev:desktop`. In dev the daemon is resolved from
`PATH` (whatever `pnpm install:th` last installed), not bundled.

## Build an installer

```bash
pnpm dist          # current platform, auto-discovered signing identity
pnpm dist:mac      # macOS, signed as $SIGN_IDENTITY (default: Apple Distribution: Smoo LLC (DTX9733844))
pnpm notarize      # notarize + staple release/*.dmg — no-ops without credentials
```

`dist` first runs `stage-daemon`, which copies this host's `smooth-daemon` into
`resources/current/` for electron-builder to bundle. Artifacts land in `release/`.
Stage a **real** daemon build — the web SPA is embedded into that binary at
compile time, so a daemon built without `pnpm build:web` serves a placeholder
page and the app window comes up blank. `pnpm install:th` from the repo root
does both in the right order.

- **macOS** → `.dmg` + `.zip`, hardened runtime, entitlements in
  `build/entitlements.mac.plist`, icon from `scripts/macos/BigSmooth.icns`. The
  bundled daemon is a nested Mach-O and electron-builder signs it separately, so
  notarization has nothing to reject. Notarization reuses
  `scripts/macos/notarize-and-staple.sh` (credentials per its README) and needs a
  **Developer ID** identity — an Apple Distribution build signs and runs but
  cannot be notarized.
- **Windows** → NSIS installer. Authenticode signs when `WIN_CSC_LINK` +
  `WIN_CSC_KEY_PASSWORD` are set, unsigned otherwise. Untested.
- **Linux** → AppImage. Untested.

## What it does

- **Daemon lifecycle.** On launch it probes `http://127.0.0.1:8787/health` (or
  `$SMOOTH_ADDR`). If a daemon already answers — `th up`, a launchd unit — it
  attaches and never touches it. Otherwise it spawns the bundled
  `smooth-daemon run` and terminates that child on Quit. Resolution order:
  bundled resources → `$SMOOTH_DAEMON_BIN` → `~/.smooth/bin` → `PATH` → the cargo
  target dir.
- **Window.** A `BrowserWindow` on the daemon's `/`. The daemon serves smooth-web
  with its local auth token already injected into `index.html`, so there is no
  renderer, preload, or IPC code here. Closing hides to the tray; Quit exits.
- **Tray.** The `th` mark, with Open / Set Up / Quit.

## TCC (macOS permissions) — known gap

**Calendar and Reminders do not prompt yet.** Measured, not assumed:

| Setup                                                                                                                 | Result                          |
| --------------------------------------------------------------------------------------------------------------------- | ------------------------------- |
| `smooth-daemon tcc calendar` spawned as a child of the signed Electron app, all usage strings in the app's Info.plist | `not-determined`, **no prompt** |
| The _identical binary_, same signature, as an app bundle's `CFBundleExecutable`, launched via `open`                  | prompt appears correctly        |

The usage strings on the Electron bundle are necessary but not sufficient: a
spawned child inherits grants the responsible app already has, but it is not
allowed to _ask_. Asking appears to require being an app bundle's main executable
launched through LaunchServices. Two candidate fixes, neither in this PR:

1. **Nested helper app.** Ship the daemon as `Big Smooth Helper.app` inside
   `Contents/Resources/` — which is what `scripts/macos/make-app-bundle.sh`
   already builds — and `open` it. Grants then attribute to the helper's bundle
   id rather than the Electron app's, which also means the long-running server
   has to be launched the same way in order to _use_ them, so daemon lifecycle
   management changes.
2. **Native module.** Call EventKit from the Electron main process. Correct
   attribution, at the cost of a compiled native dependency.

Messages (Apple Events) is the same shape — it did not prompt from a spawned
`osascript` either. Full Disk Access has no prompt by design; that tray item opens
the System Settings pane and reveals the app.

Until this is settled, the native `Big Smooth.app` (`scripts/macos/`) still holds
the working grants on a machine.

## Not done yet

Windows and Linux have never been run — packaging is configured, not verified.
Notarization is wired but unexercised (no Developer ID certificate on the build
machine used so far). Cross-compiled release artifacts need a CI job that fills
`resources/current/` for each target.
