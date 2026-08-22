# smoo-hub ops scripts

Scripts for the smoo-hub box (Apple Silicon Mac, tailnet host).

## Big Smooth daemon launchd agent

`install-smooth-daemon.sh` + `com.smooai.smooth-daemon.plist` install the Big
Smooth daemon (`/Users/brentrager/smooth-daemon run`, bound `127.0.0.1:8788`,
tailscale-served at `:8443`) as a launchd agent under the user's GUI session.

**What it does:** replaces the fragile hand-started `nohup` process with a
`KeepAlive` + `RunAtLoad` agent — the daemon survives reboots and auto-respawns
on crash. The installer first `pkill`s any lingering nohup daemon so the port is
free, then boots out any prior agent, copies the plist, and bootstraps/enables/
kickstarts it.

**Run:**

```bash
ssh smoo-hub 'cd ~/dev/smooai/smooth && ./scripts/smoo-hub/install-smooth-daemon.sh'
```

Then verify:

```bash
launchctl print gui/$(id -u)/com.smooai.smooth-daemon | head
curl -fsS http://127.0.0.1:8788/health
tail -f ~/.smooth/daemon.log
```

**Caveat — GUI session required:** `launchctl bootstrap gui/$UID` needs the
user's GUI/Aqua session to be active. It works over SSH while Brent is logged
into the console, but from a truly headless boot (no console login) the
bootstrap can't attach — `RunAtLoad` then fires on the next console login. For a
persistently-headless box you'd need a LaunchDaemon (system domain) instead;
this agent targets the logged-in-console reality of smoo-hub.

**Sibling pattern:** this mirrors `scripts/smoo-hub/install-docker-watchdog.sh`
(+ `com.smooai.smoohub.docker-watchdog.plist`) in the **smooai** repo, which
uses the same bootout → cp → bootstrap → enable → kickstart flow.

## Deploying a new Big Smooth build — `deploy.sh`

`install-smooth-daemon.sh` installs the launchd _agent_; it does not build or
ship the app. `deploy.sh` does that end-to-end, from your **build machine**
(laptop), not the hub:

```bash
scripts/smoo-hub/deploy.sh                 # build → package app → sign → ship → restart
scripts/smoo-hub/deploy.sh --dry-run       # print the plan only
SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" \
  scripts/smoo-hub/deploy.sh               # upgrade to Developer ID later
```

**The daemon ships as `Big Smooth.app`, not a bare binary (pearl th-f4baa5).**
A bare CLI can't declare Info.plist usage strings, so it can't trigger native TCC
prompts (silent EPERM) or request Calendar/EventKit at all. `deploy.sh` packages
the daemon with `scripts/macos/make-app-bundle.sh` (+ `scripts/macos/Info.plist`)
and installs it to `~/Applications/Big Smooth.app`, so on first workspace/Calendar
access macOS shows a **"Big Smooth wants to access…"** prompt — click Allow. That
same bundle builder is generic and reusable by a future user-facing installer.

**Stable code-signing is the other half (pearl th-56ee9f).** Ad-hoc signatures
(Rust's default) have a cdhash-based designated requirement that changes every
build, so each deploy would trip `OS_REASON_CODESIGNING`, trip a stale launchd
LWCR, and break any granted permission. `deploy.sh` signs the **bundle** and `th`
with a stable team identity + **fixed identifier** `ai.smoo.smooth-daemon`
(`ai.smoo.th` for th), so the DR is constant across rebuilds — grants survive and
the churn stops. **Never change those identifiers** — grants are keyed to them.
(A grant made to the earlier bare binary carries over: same identifier + cert =
same DR.)

**One-time human steps** (can't be scripted):

- _Build machine:_ the first `codesign` pops a keychain prompt to use the private
  key — click **Always Allow** once; future signs are headless.
- _Hub:_ on first access Big Smooth prompts for the workspace's external volume —
  click **Allow** at the console. `th doctor --fix-fda` remains a manual fallback.
  Thanks to the stable signature it persists across every future `deploy.sh`.

`deploy.sh` keeps a timestamped `Big Smooth.app.bak-<ts>` (and `th.bak-<ts>`) on
the hub, so a bad deploy is one `mv` away from rollback.

## Shipping the app to other people — see `scripts/macos/README.md`

`deploy.sh` is the _hub_ path (SSH + launchd, Apple Distribution signing). The
user-facing path — DMG, hardened runtime, notarization, and the release job that
does all three — lives in [`scripts/macos/README.md`](../macos/README.md) (pearl
th-a647da). Both sit on the same `make-app-bundle.sh`, and the hub deploy signs
exactly as it did before: hardened runtime turns on only for a `Developer ID`
identity.
