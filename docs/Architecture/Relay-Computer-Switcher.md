# Computer switcher — driving another computer's Big Smooth over the Smoo Relay

> Pearl `th-a49e21`. Builds on [[../Decisions/ADR-007-smoo-relay-remote-control|ADR-007]] (the Smoo Relay) and closes two of its open consequences: several daemons per user get a picker, and REST crosses the relay.

Big Smooth runs on each of your Macs (for Brent: `marvin`, the laptop, and `smoo-hub`, the always-on box). The desktop app's window always loads from the daemon on **this** computer. The **computer switcher** at the top of the sidebar lists:

- **This computer**, by name (the daemon's relay label, i.e. its short hostname), and
- every **other Big Smooth daemon** on your Smoo Relay, with its online state.

Picking one makes the window drive that computer's Big Smooth. The conversation list, `/cd` and `/pwd`, Plan/Auto, Stats, `@`-mention search, and the safety-judge settings all become that computer's. The status line reads `… on smoo-hub` and the window title becomes `Big Smooth — smoo-hub`, so where the agent is running is never ambiguous. The switcher is part of the web SPA, so it also works in the browser PWA, not only the Electron app.

## Shape

```
window (SPA)                     this computer's daemon                 Smoo Relay          remote daemon (smoo-hub)
────────────                     ──────────────────────                 ──────────          ────────────────────────
GET /api/relay/peers ──────────▶ RelayDirectory ── list_peers ────────▶ presence ───┐
                                                  ◀── {type:peers} ──────────────────┘
/api/relay/peers/<dev>/ws ─────▶ Dialer: own relay socket ─────────────▶ {to,frame} ──▶ main relay socket
   (canonical operator WS)        as <self>-w<slot>, kind=phone          ◀── {from,frame} ◀── per-peer loopback bridge
                                                                                          → its own operator /ws
/api/relay/peers/<dev>/<route> ▶ HttpLinks: one socket per remote ─────▶ {channel:http} ─▶ relay_http::Server
   (/api/session/cwd, /api/stats…)  request/response by id                               → its own loopback REST
```

The window never learns anything new: it sets its API base to `/api/relay/peers/<device>` instead of `/`, and every `${http}/…` call and the `/ws` socket follow ([`computers.ts`](../../crates/smooth-web/web/src/computers.ts) `apiBase`). The choice is made once, before the first render ([`main.tsx`](../../crates/smooth-web/web/src/main.tsx)); switching remembers the pick in `localStorage` (`smooth.computer`) and reloads.

### Why the local daemon is the relay client

The daemon already holds the Smoo session (kept fresh by the credential heartbeat) and already speaks the relay. Having it tunnel for the window means:

- no second sign-in in the desktop app, and the Supabase token never reaches the web view;
- the same code serves the Electron window and the browser PWA;
- the remote side needs no new trust: to smoo-hub, the window's tunnel is a relay client of the same user, bridged into its operator exactly like a phone ([`relay.rs`](../../crates/smooth-daemon/src/relay.rs) `spawn_bridge`). Chat frames on the operator channel are not end-to-end encrypted; that is true for phones today too (only `channel:"flow"` frames are, per `th-d98fde`).

The alternative, the Electron main process dialling the relay as its own phone-kind client, would have needed its own login, duplicated the relay client in TypeScript, and helped only the desktop app.

### Why each window gets its own relay socket

A daemon's main relay socket treats every inbound envelope as a request to drive it. If the window's traffic rode that socket, replies from smoo-hub would arrive on marvin's main socket and be bridged into marvin's own operator, and two daemons driving each other would loop. A separate socket per window WebSocket ([`relay_tunnel.rs`](../../crates/smooth-daemon/src/relay_tunnel.rs) `Dialer`) makes direction structural: a tunnel socket only ever carries replies for its window, and the main socket only ever carries requests. Tunnel device ids are `<this daemon's id>-w<slot>` with the lowest free slot, so a reconnecting window comes back as the same device and the remote reuses its bridge instead of growing one per reconnect. Tunnels dial as `kind=phone`, so no picker ever offers one as a computer.

### REST over the relay

The relay carries only WebSocket frames, so a REST call travels as one frame on its own channel ([`relay_http.rs`](../../crates/smooth-daemon/src/relay_http.rs)):

```json
{"channel":"http","type":"http.request","id":"…","method":"GET","path":"/api/session/cwd?session=abc","body":null}
{"channel":"http","type":"http.response","id":"…","status":200,"content_type":"application/json","body":"{…}"}
```

The remote daemon answers by calling its own loopback server with its own local token, the same seam its operator bridges use. REST calls to one remote computer share one tunnel, multiplexed by id and closed after 90s idle.

## Security

- **Same user only.** The relay routes only between devices of the same Smoo user, by construction. On top of that, this daemon will only tunnel to a device the relay lists right now as one of that user's `daemon` peers. It refuses this computer itself, phones, SmoothFlow's `flow` child, its own tunnels, and any id outside the relay grammar `[A-Za-z0-9._-]{1,64}` (tests: `only_a_listed_daemon_of_this_user_can_be_targeted`, `another_users_device_is_unreachable`).
- **Local gate.** Every `/api/relay/peers*` route requires this daemon's local token, like the rest of its API.
- **Nothing local leaves.** The window's `?token=` is stripped before a path is forwarded (`relay_http::strip_token`), and no request header crosses the relay. The Supabase session signs only the relay dial.
- **Exact allowlist.** Only these `(method, path)` pairs cross, checked on both ends: `GET /admin/me`, `GET /admin/model-costs`, `GET|POST /api/session/cwd`, `GET|POST /api/session/mode`, `GET /api/stats`, `POST /api/usage`, `GET|POST /api/judge`, `GET /search`, `GET /api/mode`, `GET /api/skills`, `GET /api/plugins`, `GET /api/model-catalog`, `GET /api/llm/provider`. Nothing under `/api/flow` (shells, with their own E2E channel), `/api/relay` (no onward hops), `/auth`, or `/push`. An exact match also rules out traversal (`/api/stats/../flow`, `%2e%2e`, `//host`). Only GET and POST are accepted, bodies are capped at 1 MiB, the loopback call bypasses any `HTTP(S)_PROXY`, and a daemon serves at most 16 relay requests at once.
- **No new privilege.** A same-user peer that can send an `http.request` can already drive the operator itself (bash included), so the allowlist narrows the surface rather than guarding a boundary.

## When a computer is unreachable

This computer always works. The switcher always renders it, even when `/api/relay/peers` reports the relay down.

| Situation                             | What the window does                                                                                                                                                              |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Signed out of Smoo / session expired  | Only this computer is listed; the footer says to sign in                                                                                                                          |
| Relay off (`SMOOTH_RELAY=0`)          | Same, "The Smoo Relay is turned off on this computer."                                                                                                                            |
| Remembered computer offline at launch | Comes up on this computer, with an amber line: "smoo-hub isn't reachable — …. Showing this computer." The remembered pick stays in the list as an unreachable row with the reason |
| Remote drops mid-session              | Its socket closes with code 4404 and a reason; the window shows disconnected and retries. Retries hit a 10s peer cache, not the relay                                             |
| Remote daemon too old to answer REST  | Chat still works (its operator bridge predates this). REST calls time out after 25s with "its Big Smooth may need an update"                                                      |

## Routes

| Route                                         | Purpose                                                                                                                                                 |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/relay/peers`                        | `{self:{device,label}, relay:{state,detail,since}, peers:[{device,label,kind}], error}`. Always 200 once authorised, so the switcher can explain itself |
| `GET /api/relay/peers/{device}/ws`            | That computer's canonical operator WebSocket, tunnelled. Dialled before upgrading, so relay failures are HTTP errors (400/404/502/503/504)              |
| `GET\|POST /api/relay/peers/{device}/{*path}` | That computer's REST route `/{path}`, allowlisted                                                                                                       |

## Tests

- `relay_http` covers the allowlist, including adversarial paths; token stripping; frame round trips; and a real loopback server showing the remote uses its own token.
- `relay_tunnel` uses an in-memory relay with the real routing rules to cover: signed-out and unreachable dials; ack timeout; target-only delivery (a stranger on the same account is ignored); another user's device being unreachable; silence; REST multiplexing; the idle close; and an old remote.
- `relay` covers `peers` parsing, the directory cache and timeout, the live link answering `list_peers`, and HTTP frames served through the real connection loop.
- `relay_peers_route` covers the gate on every route, list filtering, offline and disabled relays, target validation, and one end-to-end test: window → local routes → fake relay → the remote's real `run_connection` → its operator WS and REST, asserting the local token never leaves.
- In the SPA, `computers.test.ts` covers the remembered pick, fallback rules, reasons, rows, and the API base.

## Not in v1

- **Phones.** The Big Smooth iOS and Android apps could use `channel:"http"` for Stats and REST over the relay. ADR-007 left that as a follow-up. Pearl `th-3cfb0b`.
- **Tray menu.** The Electron tray's **Connect** submenu still lists tailnet daemons only. The relay switcher lives in the window.
