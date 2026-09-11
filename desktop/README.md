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

- **Daemon lifecycle.** On launch it **always** probes the LOCAL daemon
  (`http://127.0.0.1:8787/health`, or `$SMOOTH_ADDR` / the addr in
  `~/.smooth/daemon.addr`). If one already answers — `th up`, a launchd unit, a
  login-item instance — it attaches and never touches it. Otherwise it spawns the
  bundled `smooth-daemon run` and terminates that child on Quit. Resolution order:
  bundled resources → `$SMOOTH_DAEMON_BIN` → `~/.smooth/bin` → `PATH` → the cargo
  target dir. The app's own spawn/update diagnostics go to `~/.smooth/desktop.log`
  — a Finder/`open` launch has no terminal, so that file is where a startup
  failure actually shows up (th-5c2ec6). The daemon's output goes to the log
  folder below.
- **Daemon supervision (th-4b189c).** A child the app spawned is watched for as
  long as the app runs. The app once ran for hours with its daemon dead — nothing
  on the port, no log of the exit anywhere — because the `exit` handler only
  cleared a variable. Now:
    - **Exit → logged + respawned with backoff.** The exit code/signal, the
      uptime, and the last 40 stderr lines land in `daemon.log`; the child is
      respawned after 1s, 2s, 4s … capped at 60s. Eight consecutive failures
      (about three minutes) and it gives up and asks the user. A child that stays
      healthy for a minute resets the streak, so an isolated crash next week
      starts at 1s again, not at the cap.
    - **Hung ≡ dead.** Every 30s the app probes `GET /api/mode` (a real router
      round-trip, not a TCP accept). Two consecutive misses (5s timeout each)
      → SIGTERM, SIGKILL after 4s, then the same respawn path. An _attached_
      daemon (one the app didn't start) is probed too so the tray can say it's
      gone, but is never killed or respawned — it isn't ours.
    - **Tray + About.** The second tray line is the daemon's: `Daemon: running ·
up 2h · 1 restart`, `Daemon crashed — restarting in 4s (attempt 2/8)`
      (click to skip the wait), or `Daemon stopped — click to retry`. **About
      Big Smooth…** shows app + daemon versions, pid, address, uptime, restarts
      this session, the last exit, and an **Open Logs** button. When a respawned
      daemon comes back the window reloads so the SPA reconnects instead of
      sitting on a dead WebSocket; a start that never came up opens the window
      on first health instead of quitting the app.
    - **Quit/OTA.** `stopDaemon()` tells the supervisor first, so the deliberate
      exit is neither logged as a crash nor respawned into the bundle the
      updater is about to swap (th-79416c still holds).
    - The decisions are pure (`src/supervisor.ts`, tests in
      `supervisor.test.ts`); `src/daemon.ts` is the plumbing.
- **Logs.** `~/Library/Logs/Big Smooth/` (macOS; `~/.smooth/logs/` elsewhere;
  `SMOOTH_DESKTOP_LOG_DIR` overrides), size-rotated (`.1`…`.3`):
    - `daemon.log` — the child's stdout/stderr plus `[supervisor]` lines
      (spawns, exits with code/signal/uptime, the stderr tail, hung-kills,
      give-ups). Written by the app (`src/daemonlog.ts`).
    - `smooth-daemon.log` — the daemon's own tracing. The app passes
      `SMOOTH_LOG_FILE=<that path>` when it spawns, so the daemon writes there
      (no ANSI, rotated at 10 MB on startup) instead of to stderr; only panics
      and the one-line `logging to …` breadcrumb reach stderr → `daemon.log`.
      Any launcher can set `SMOOTH_LOG_FILE` (launchd, `nohup`); unset keeps the
      stderr default. `RUST_LOG` still picks the filter.
- **Local vs remote is a view target, not a daemon switch (th-5c2ec6).** Connecting
  to a remote daemon (tray → Connect → a tailnet peer) only changes what the
  **window** loads; this Mac still runs its own local daemon in the background, so
  the phone/relay and scheduled turns keep working. A stale saved `remoteUrl` can
  no longer silently leave the machine with no local daemon. The current mode is
  shown as the first (disabled) line of the tray menu and in the window title.
- **Open at Login (th-ccf2cf).** On first run the app enables
  `app.setLoginItemSettings({ openAtLogin: true })` (macOS `SMAppService`) so the
  daemon comes back after a reboot without a hand-made plist. It's applied exactly
  once (tracked by `loginItemConfigured` in the userData `config.json`); after
  that the tray's **Open at Login** checkbox (or System Settings) owns it.
- **Updates (OTA, th-75eb2e).** electron-updater polls the feed at launch + every
  30 min (`src/updater.ts`; the pure decisions are in `src/updateDecision.ts` with
  tests). `autoDownload` is **off** — when an update is _available_ the app shows a
  native, Sparkle-style choice dialog (Electron's own `dialog.showMessageBox`,
  since Electron can't use Sparkle the way the native SmoothFlow companion does):
  title "A new version of Big Smooth is available!", the "X is now available—you
  have Y" body, a **Skip This Version / Remind Me Later / Install Update** button
  row, and an "Automatically download and install updates in the future" checkbox.
    - **Install Update** → `downloadUpdate()`; when the download completes the
      existing guarded restart step ("Restart now / Later" → stop daemon →
      `quitAndInstall`, th-79416c) takes over.
    - **Skip This Version** persists the version to `skippedUpdateVersions` in the
      userData `config.json` — it's never offered again in the background.
    - **Remind Me Later** just defers; the next launch / 30-min poll re-offers.
    - The **checkbox** persists `autoUpdate` in `config.json`; once set, a future
      available update downloads silently (still landing on the guarded restart
      prompt) instead of showing the choice dialog. Tray → **Check for Updates…**
      always shows the dialog, even for a skipped version, and reports the
      up-to-date case. All of it flows through the same attempt-cap / give-up /
      once-per-session guards as before (th-d4feb8), so a bundle that won't install
      still falls back to a manual download instead of nagging forever.
- **Window.** A `BrowserWindow` on the daemon's `/`. The daemon serves smooth-web
  with its local auth token already injected into `index.html`, so there is no
  renderer, preload, or IPC code here. Closing hides to the tray; Quit exits.
- **Tray.** The `th` mark, with the current-mode header, the daemon status line,
  Open / Open at Login / Check for Updates / About / Set Up / Connect / Quit.
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
