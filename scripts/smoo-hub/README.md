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
