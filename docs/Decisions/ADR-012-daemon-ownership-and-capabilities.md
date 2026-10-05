---
status: Accepted
date: 2026-10-05
pearl: th-4a858d
---

# ADR-012 — Daemon ownership and CLI↔daemon compatibility

## Status

Accepted (2026-10-05, pearl th-4a858d; subsumes th-db89de).

## Context

Two programs ship a Big Smooth daemon (`smooth-daemon`):

- **`th`**, from Homebrew, the install script or `cargo install`. `th up` and
  `th code`'s autostart run the daemon in-process. This is the only daemon on
  Linux, on servers (smoo-hub) and for CLI-only users.
- **Big Smooth.app**, the macOS desktop app. It bundles its own daemon and
  ships on its own schedule through a manual OTA (`desktop-publish.yml`).

They release independently. On 2026-10-05, `th` 0.72.1 shipped per-session
workspaces (`POST /api/session/workspaces`, th-adc3d4). The app on the
developer's Mac was still running its bundled daemon **0.54.0**. `th code`
POSTed to the new route, and the old daemon answered with the web UI's
`index.html` and **200 OK**: the SPA router serves `index.html` for every
unmatched path, `/api/*` included. `th code` read that as success. Adding
repos, inheriting PATH and the new diagnostics were all silently inactive, and
nothing told the user why.

Three gaps caused that:

1. **No ownership rule.** The single-instance lock (`single_instance.rs`) stops
   two daemons from _running_ at once. It does not say _which_ program should
   provide the daemon. With the app installed but quit, `th code` autostarts a
   CLI daemon. When the app launches later, it loses the lock race, and the
   user gets whichever version happened to start first.
2. **No version negotiation.** Clients assume the daemon has every route they
   were compiled against. That only holds in lockstep, and the app and `th`
   are not in lockstep.
3. **The SPA fallback hides missing routes.** A 200 HTML reply to an API call is
   indistinguishable from success unless every client validates every body.

## Decision

### (a) One daemon per machine, owned by the app when the app is installed

The single-instance lock stays. Before a client _starts_ a daemon (`th up`,
`th code` autostart), it decides who owns the daemon:

| Big Smooth.app         | Override set | Decision                                                                                    |
| ---------------------- | ------------ | ------------------------------------------------------------------------------------------- |
| not installed          | —            | **CLI owns it.** `th` starts its own daemon, as before (Linux, servers, CLI-only users).    |
| installed, running     | no           | **App owns it.** Use the app's daemon and wait for it to come up; never start a second one. |
| installed, not running | no           | **App owns it.** Launch the app (`open`), tell the user, and use its daemon once healthy.   |
| any                    | yes          | **CLI owns it.** Headless, CI and dev setups that want their own daemon.                    |

A live daemon at the advertised address is always used, whoever started it.
The decision applies only when nothing is listening.

The override is the `daemon.prefer_own` setting (`th settings set
daemon.prefer_own true`, env `SMOOTH_PREFER_OWN_DAEMON`).
`SMOOTH_ALLOW_SECOND_DAEMON=1`, the existing multi-instance escape hatch,
implies it.

"Installed" means `~/Applications/Big Smooth.app` or `/Applications/Big
Smooth.app` exists. "Running" means a process runs from inside that bundle
(`Big Smooth.app/Contents/`). The decision is a pure function over an
injectable probe, so it is unit-tested without an app on the machine.

### (b) Capability handshake instead of lockstep releases

The daemon serves `GET /api/capabilities`:

```json
{ "version": "0.73.0", "capabilities": ["session.cwd", "session.mode", "session.workspaces"] }
```

- Capabilities are short dotted strings, named in one shared table
  (`smooth_policy::daemon`) together with the first daemon version that has
  each one. That version is what clients quote in upgrade hints.
- Clients **feature-detect**. They never compare version numbers to decide
  behavior.
- A daemon without the endpoint (404, non-JSON, or an HTML 200 from an old SPA
  fallback) is treated as having **no capabilities**. A pre-ADR daemon
  degrades cleanly instead of being misread.
- A missing capability turns off only its own feature. Everything else keeps
  working. `th code` names the missing capability, the minimum daemon version
  and where the update comes from: "update from the Big Smooth app menu" when
  the app owns the daemon, `brew upgrade th` when the CLI does.

### (c) Unknown `/api/*` routes return a JSON 404

The SPA fallback answers any path under `/api/` it does not know with `404`
and a JSON body (`{"error":"not_found","path":"/api/…"}`). It never serves
`index.html` for those paths. The SPA keeps client-side routing for every other
path. This removes the root cause. The handshake in (b) covers daemons that
predate the fix.

### (d) Independent release cadence, with a support window

- **The app releases on its own cadence**, driven by daemon capabilities: ship
  an app build within one week of a `th` release that adds a capability a
  released client uses, and at least monthly otherwise. `th` releases are never
  held for the app.
- **Support window: older daemons.** `th` must work against any daemon that
  speaks the canonical operator WebSocket protocol, including capability-less
  daemons that predate this ADR. For those it degrades to core chat. Features
  gated on a capability must keep a clear "needs Big Smooth ≥ X" path for at
  least **6 months** after the capability first shipped. Dropping a
  capability-less fallback needs a new ADR.
- **Support window: older clients.** A daemon keeps every route a released `th`
  uses for at least **6 months** after a replacement ships. It keeps a
  capability string in the list for as long as it serves the feature behind it.
  Capability names are never reused for different behavior.

## Reasoning

- **Users run both programs.** A per-feature capability check is cheaper than
  forcing app and CLI releases into lockstep. Lockstep is impossible anyway for
  CLI-only users, Linux and servers. Version comparisons would encode the
  release history of two products into every client.
- **The app is the better owner on a Mac.** It holds the macOS TCC grants
  (Calendar, Reminders, Contacts), the menu bar and the OTA updater. A CLI
  daemon that wins the lock race silently loses those grants, which was the
  original th-c71e6f failure.
- **Silent success is the worst outcome.** A loud "needs ≥ X" is better than a
  feature that appears to work. (c) fixes it at the source. (b) makes clients
  robust against daemons that cannot be fixed retroactively.

## Implementation

- `crates/smooth-policy/src/daemon.rs`: the ownership decision, the system
  probe, and the capability table with its wire type.
- `crates/smooth-daemon`: `GET /api/capabilities`.
- `crates/smooth-web/src/lib.rs`: JSON 404 for unknown `/api/*` in the SPA
  fallback.
- `crates/smooth-code/src/client.rs`: `capabilities()`. `open_workspace`
  rejects a non-JSON reply. `ensure_server` applies the ownership decision
  before autostarting.
- `crates/smooth-code/src/app.rs`: per-session workspaces gated on
  `session.workspaces`, with a one-time warning when the capability is missing.
  `/status` shows the daemon version and capabilities.
- `crates/smooth-cli/src/main.rs`: `th up` applies the ownership decision.
- `crates/smooth-policy/src/settings.rs`: `daemon.prefer_own`.

## Consequences

### Positive

- Mixed versions degrade visibly and gracefully, instead of failing silently.
- Mac users get one daemon that keeps the app's TCC grants, whichever command
  they ran first.
- The app and `th` can release independently.

### Negative

- Each new client-facing daemon feature needs a capability string and an entry
  in the shared table.
- `th` can now launch a GUI app as a side effect of `th up` or `th code` when
  the app is installed but not running. Headless macOS setups must set
  `daemon.prefer_own`.

### Neutral

- Daemons that predate this ADR report no capabilities, even for features they
  do have (0.72.1 serves workspaces but not the endpoint). They get the
  degraded path until they update. This is accepted: one release of overlap,
  and it errs toward a visible message.

## Alternatives Considered

### Lockstep releases (the app always bundles the newest `th`)

Rejected. The app ships through a manual, notarized OTA. Gating every `th`
release on that pipeline couples two cadences. It does nothing for users
already running an older app, which is exactly the failure that motivated this
ADR.

### Compare the daemon's version number in each client

Rejected. Clients would carry a "feature X since version Y" table anyway and
also break on dev builds and pre-release versions. Capabilities state what the
daemon can do directly, and the version is only used for the upgrade message.

### `th` always runs its own daemon and the app becomes a client

Rejected for macOS. The TCC grants belong to the app bundle, and a CLI daemon
cannot hold them (th-c71e6f).

## Related

- [[Decisions/ADR-Index]]
- [[../Architecture/Daemon-Direction]]
- [[../Engineering/Using-th-CLI]]
- Pearls: th-4a858d, th-db89de, th-adc3d4, th-c71e6f (single-instance lock)
