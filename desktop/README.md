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

`dist` first runs `stage-daemon`, which copies this host's `smooth-daemon` **and**
`th` into `resources/current/` for electron-builder to bundle. Artifacts land in
`release/`. Stage a **real** daemon build — the web SPA is embedded into that
binary at compile time, so a daemon built without `pnpm build:web` serves a
placeholder page and the app window comes up blank. `pnpm install:th` from the
repo root does both in the right order.

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
- **`th` on PATH.** The DMG bundles the `th` CLI next to the daemon. On launch the
  app symlinks it into a PATH dir (`/usr/local/bin/th`, falling back to
  `~/.local/bin/th`) so `th` works from a terminal. Because the link points **into
  the app bundle**, an OTA update that replaces the bundle auto-updates the `th`
  users run — no separate CLI update channel. **Coexistence rule** (mirrors
  `scripts/dev-link-th.sh`): the app only ever creates or repoints a **symlink**.
  A regular file at the target — a `th` you installed with Homebrew or the curl
  installer — is left untouched, never clobbered; the app logs and defers to it.
  Logic + tests in `src/installth.ts`.

## TCC (macOS permissions)

macOS shows the EventKit prompt only for a signed bundle whose _main executable_
asks. Measured, not assumed:

| Setup                                                                                                                 | Result                          |
| --------------------------------------------------------------------------------------------------------------------- | ------------------------------- |
| `smooth-daemon tcc calendar` spawned as a child of the signed Electron app, all usage strings in the app's Info.plist | `not-determined`, **no prompt** |
| The _identical binary_, same signature, as an app bundle's `CFBundleExecutable`, launched via `open`                  | prompt appears correctly        |

The Electron app's main executable is Electron, and it spawns `smooth-daemon` as
a child — a spawned child inherits grants the responsible app already has, but it
is not allowed to _ask_.

**The fix (nested helper app, pearl th-fd06bf).** `scripts/after-pack.mjs`
assembles a tiny helper bundle at `Contents/Helpers/BigSmoothTCC.app` whose
`CFBundleExecutable` IS `smooth-daemon` (a copy of the same bundled binary),
with the Calendar/Reminders usage strings and a stable bundle id
(`ai.smoo.smooth-daemon`, matching the native bundle's TCC key). The hook runs
before signing, so electron-builder's `@electron/osx-sign` signs the nested
bundle with the app's Developer ID + hardened runtime (the same recursive
signing that already covers the bundled `smooth-daemon`/`th` under
`Contents/Resources`), and it notarizes. The tray's **Set Up → Calendar…/Reminders…** and
`th doctor --setup-calendar`/`--setup-reminders` launch it via
`open -n <helper> --args tcc <what>`, which prompts. `open` can't return the
child's stdout, so `grantEventKit()` then polls `smooth-daemon tcc <what>` (as a
child — reading status works even though asking doesn't) for the result.

**Manual verification** (needs a GUI login session + a signed/notarized build —
CI does the signing):

1. Install the built `Big Smooth.app`.
2. Tray → **Set Up → Calendar…** — macOS should show
   "Big Smooth would like to access your calendar"; choose **Allow**.
3. Confirm in System Settings → Privacy & Security → Calendars.
4. Ask Big Smooth "what's on my calendar today?" (the daemon shells `ical`), or
   run `ical today` — it should now return events. Repeat with **Reminders…**.

Messages (Apple Events) is the same shape and is still driven by a spawned
`osascript`; if it needs the same treatment, route it through the helper too.
Full Disk Access has no prompt by design; that tray item opens the System
Settings pane and reveals the app.

## Not done yet

Windows and Linux have never been run — packaging is configured, not verified.
Notarization is wired but unexercised (no Developer ID certificate on the build
machine used so far). Cross-compiled release artifacts need a CI job that fills
`resources/current/` for each target.
