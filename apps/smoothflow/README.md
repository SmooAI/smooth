# SmoothFlow (macOS)

The native fleet console for Smooth agents. AppKit + libghostty, macOS 14+.
Architecture, the TCC matrix and the reasoning behind the daemon lifecycle:
[`docs/Architecture/SmoothFlow-macOS.md`](../../docs/Architecture/SmoothFlow-macOS.md).

## Build

```bash
brew install xcodegen
cd apps/smoothflow
xcodegen generate                                   # SmoothFlow.xcodeproj (gitignored)
xcodebuild -project SmoothFlow.xcodeproj -scheme SmoothFlow -configuration Debug \
    -derivedDataPath build/DerivedData build        # also fetches the pinned GhosttyKit
xcodebuild … test                                   # XCTest (frame codec, reducer, notifier, address)
open build/DerivedData/Build/Products/Debug/SmoothFlow.app
```

`scripts/ensure-ghosttykit.sh` downloads the prebuilt `GhosttyKit.xcframework`
pinned in `scripts/ghosttykit.lock` (manaflow-ai/ghostty fork — it has the
manual I/O surface mode upstream lacks) into `Vendor/` and refuses a checksum
mismatch. Bump both lines of the lock together.

`Info.plist` and `entitlements.plist` are hand-maintained and referenced by
build settings. Do **not** add xcodegen's `info:`/`entitlements:` keys: they
regenerate the files empty, and every TCC prompt then silently fails.

Release: `SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" scripts/build-release.sh`
(bundles `~/.cargo/bin/smooth-daemon`, signs, notarizes when `NOTARY_*` are set,
writes `dist/SmoothFlow.dmg`). CI: `.github/workflows/smoothflow-mac.yml`.

## Run against the mock engine

The engine (lane A) may not be on your machine. The mock speaks the v0 flow
protocol (`mock/protocol.md`) with the wireframes' fixture fleet:

```bash
node apps/smoothflow/mock/server.mjs 8790
SMOOTHFLOW_DAEMON_ADDR=127.0.0.1:8790 open -n build/DerivedData/Build/Products/Debug/SmoothFlow.app
```

Typing into a surface echoes back through `flow.input` → `flow.output`;
Allow in the inbox (or ⌘⌥Y) flips the session to working then done; ⌘⇧N
fans out three candidates that finish on a timer. Settings ▸ Daemon also
takes the address.

Without the override the app spawns `smooth-daemon` itself (child mode) and
the `smoothflow` tmux server; until the engine lands the flow WebSocket will
404 and the sidebar says so.

## Permissions

Grant everything **from the app** (Settings ▸ Permissions, or the SmoothFlow ▸
Permissions menu): macOS only shows a prompt to an app bundle's main
executable, and children inherit. Never test TCC from a shell-launched
binary — a process launched from a terminal reads the terminal's grants.
`scripts/tcc-probe.sh <label>` prints what TCC thinks of the process it runs
in; run it from a pane inside `tmux -L smoothflow` to see the app's view.

Full Disk Access has no prompt: the pane deep-links to System Settings and
re-probes when the app activates.
