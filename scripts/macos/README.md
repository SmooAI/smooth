# macOS packaging — `Big Smooth.app`

Four small scripts, composed. Each one does exactly one thing and prints the
artifact it produced on stdout, so they pipe together:

| Script | What it does |
| --- | --- |
| `make-app-bundle.sh <daemon-bin> <out-dir> [version]` | Assembles + signs `Big Smooth.app` (+ bundles `th`). |
| `make-dmg.sh <app> <out.dmg> [volume-name]` | Wraps the app in a drag-to-Applications `.dmg`. |
| `notarize-and-staple.sh <artifact…>` | Apple notarization + staple. No-op without credentials. |
| `install-local.sh` | Builds everything and installs the app to `~/Applications` on THIS Mac. |

Day-to-day you only run the last one:

```bash
scripts/macos/install-local.sh --open        # build → package → install → launch
scripts/macos/install-local.sh --login-item  # …and auto-start at login
```

Building a distributable, end to end:

```bash
cargo build --release -p smooai-smooth-daemon -p smooai-smooth-cli
export SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)"
APP=$(scripts/macos/make-app-bundle.sh target/release/smooth-daemon build 0.26.5)
DMG=$(scripts/macos/make-dmg.sh "$APP" dist/BigSmooth-0.26.5.dmg "Big Smooth")
scripts/macos/notarize-and-staple.sh "$DMG"
```

CI does exactly this — `.github/workflows/macos-app.yml`, on a `v*` tag or
manual dispatch.

## Signing — the three tiers

`SIGN_IDENTITY` (default `-`, ad-hoc) decides how much of the chain runs:

| Identity | Hardened runtime | Notarizable | Use |
| --- | --- | --- | --- |
| `-` (ad-hoc) | no | no | local dev; runs on this Mac only |
| `Apple Distribution: …` | no | no | smoo-hub deploys — a *stable* DR, so TCC grants survive |
| `Developer ID Application: …` | **yes** | **yes** | anything a user downloads |

Hardened runtime (`codesign --options runtime`) turns on **only** for a
`Developer ID` identity: Apple requires it for notarization, and it buys nothing
on the other two while adding ways for a local run to break. The bundle
identifiers (`ai.smoo.smooth-daemon`, `ai.smoo.th`) are FIXED — TCC grants are
keyed to them, so changing one resets every permission the app was given.

## The bundled `th` CLI

`make-app-bundle.sh` copies `th` into `Contents/Resources/bin/th` (from `$TH_BIN`,
default: a `th` next to the daemon binary) and signs it separately — a nested
Mach-O needs its own signature or notarization rejects the bundle. The menu bar
then offers **Install th CLI…**, which symlinks it to `/usr/local/bin/th`, or
`~/.local/bin/th` when that isn't writable (the VS Code "install `code` command"
pattern). No `th` in the build → no menu item, no error.

## The app icon

`BigSmooth.icns` is checked in (built asset) and copied to
`Contents/Resources/`; `Info.plist` points at it with `CFBundleIconFile`. Source
art is the `th` PWA icon, `crates/smooth-web/web/public/pwa-512x512.png` — a
full-bleed dark square. macOS does **not** re-mask app icons, so the rounded
rect is baked in here: the square is scaled to Apple's 824×824 art area inside a
1024×1024 canvas and masked with a 22.37%-radius rounded rect. Regenerate after
a brand change:

```bash
python3 - <<'PY'
from PIL import Image, ImageDraw
S, INSET = 1024, 100
art = S - 2 * INSET
im = Image.open('crates/smooth-web/web/public/pwa-512x512.png').convert('RGB').resize((art, art), Image.LANCZOS)
mask = Image.new('L', (art * 4, art * 4), 0)   # 4x then downsample = cheap AA
ImageDraw.Draw(mask).rounded_rectangle([0, 0, art * 4 - 1, art * 4 - 1], radius=int(0.2237 * art * 4), fill=255)
out = Image.new('RGBA', (S, S), (0, 0, 0, 0))
out.paste(im, (INSET, INSET), mask.resize((art, art), Image.LANCZOS))
out.save('/tmp/icon_1024.png')
PY
mkdir -p /tmp/BigSmooth.iconset
for s in 16 32 128 256 512; do
    sips -z $s $s /tmp/icon_1024.png --out "/tmp/BigSmooth.iconset/icon_${s}x${s}.png"
    sips -z $((s*2)) $((s*2)) /tmp/icon_1024.png --out "/tmp/BigSmooth.iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns /tmp/BigSmooth.iconset -o scripts/macos/BigSmooth.icns
```

macOS caches icons aggressively — after installing, `touch "$HOME/Applications/Big Smooth.app" && killall Dock Finder`.

## Notarization credentials

`notarize-and-staple.sh` reads one of two credential sets from the environment
and **exits 0 with an explanation when neither is present**, so unsigned local
and CI builds still succeed:

```bash
# App Store Connect API key (preferred — no app-specific password, no 2FA dance)
NOTARY_KEY=~/AuthKey_ABC123.p8 NOTARY_KEY_ID=ABC123 NOTARY_ISSUER=<uuid>

# or Apple ID
NOTARY_APPLE_ID=you@smoo.ai NOTARY_TEAM_ID=DTX9733844 NOTARY_PASSWORD=<app-specific>
```

### GitHub Actions secrets

`.github/workflows/macos-app.yml` reads these; every one is optional, and the
job degrades to an ad-hoc, un-notarized DMG when they're missing.

| Secret | Value |
| --- | --- |
| `MACOS_SIGN_IDENTITY` | `Developer ID Application: Smoo LLC (DTX9733844)` |
| `MACOS_CERT_P12` | base64 of the exported `.p12` (cert **+ private key**) |
| `MACOS_CERT_PASSWORD` | the `.p12` export password |
| `NOTARY_KEY_P8` | base64 of `AuthKey_XXXX.p8` |
| `NOTARY_KEY_ID` | the key ID |
| `NOTARY_ISSUER` | the issuer UUID |

Set them with the wrapper, never `echo | gh secret set --body -` — that stores a
trailing newline and silently breaks byte-comparing consumers (SMOODEV-879):

```bash
base64 -i DeveloperID.p12 | tr -d '\n' > /tmp/cert.b64
scripts/secret-helpers/gh-secret-set MACOS_CERT_P12 "$(cat /tmp/cert.b64)" -R SmooAI/smooth
```

## Prerequisites (one-time, human)

- An Apple Developer Program membership (Team `DTX9733844`) → create a
  **Developer ID Application** certificate, export it as `.p12` with its private
  key.
- An **App Store Connect API key** with the Developer role (Users and Access →
  Integrations) → gives you the `.p8`, key ID, and issuer UUID.
- On the build machine, the first `codesign` pops a keychain prompt — click
  **Always Allow** once; later signs are headless.

Until both exist the whole pipeline still runs; the DMG is just ad-hoc signed,
and users get a Gatekeeper warning (right-click → Open) instead of a clean
double-click.
