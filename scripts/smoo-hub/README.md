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

`install-smooth-daemon.sh` installs the launchd *agent*; it does not build or
ship the *binary*. `deploy.sh` does that end-to-end, from your **build machine**
(laptop), not the hub:

```bash
scripts/smoo-hub/deploy.sh                 # build → sign → ship → restart on smoo-hub
scripts/smoo-hub/deploy.sh --dry-run       # print the plan only
SIGN_IDENTITY="Developer ID Application: Smoo LLC (DTX9733844)" \
  scripts/smoo-hub/deploy.sh               # upgrade to Developer ID later
```

**Stable code-signing is the point (pearl th-56ee9f).** Ad-hoc signatures (Rust's
default) have a cdhash-based designated requirement that changes every build, so
each deploy trips `OS_REASON_CODESIGNING`, trips a stale launchd LWCR, and breaks
any Full Disk Access grant. `deploy.sh` signs both binaries with a stable team
identity + **fixed identifiers** (`ai.smoo.smooth-daemon`, `ai.smoo.th`) so the
DR is constant across rebuilds — the FDA grant survives, and the churn stops.
**Never change those identifiers** — the grant is keyed to them.

**One-time human steps** (can't be scripted):

- *Build machine:* the first `codesign` pops a keychain prompt to use the private
  key — click **Always Allow** once; future signs are headless.
- *Hub:* the workspace is on an external volume (`/Volumes/smoo-ext`), which macOS
  TCC-gates. Grant Full Disk Access to `~/smooth-daemon` + `~/.cargo/bin/th` once
  via `th doctor --fix-fda` at the hub's console. Thanks to the stable signature
  it then persists across every future `deploy.sh`.

`deploy.sh` keeps timestamped `*.bak-<ts>` copies of the previous binaries on the
hub, so a bad deploy is one `mv` away from rollback.
