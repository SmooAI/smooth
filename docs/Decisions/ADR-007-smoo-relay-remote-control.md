# ADR-007: Smoo Relay — phone remote control without tailscale

- **Status**: Accepted
- **Date**: 2026-08-07
- **Pearls**: th-5561c5 (epic), th-2f626d (daemon client); smooai SMOODEV-2828 (relay service), SMOODEV-2829/2830 (native apps)

## Context

Big Smooth (smooth-daemon) binds loopback and is reachable off-box only via
`tailscale serve` — tailnet-private by design. The Big Smooth native mobile
apps (iOS/Android) need to control the daemon from anywhere, and requiring
every user's phone to join a tailnet is a non-starter for a consumer surface.

Constraints that shaped the decision:

- The daemon's LocalServer exposes exactly one client seam: the canonical
  smooth-operator WS at `/ws?token=…` (strict auth, loopback). There is no
  in-process client API — the scheduler already talks to the daemon as a
  loopback WS client for this reason.
- smooai already runs `rust/events-ws`: an axum WS service with fail-closed
  Supabase-JWKS auth and a per-identity Valkey pub/sub backplane, deployed
  behind a WS-correct ALB. Both the phone (after Smoo sign-in) and the daemon
  (`th auth login` session, heartbeat-refreshed) already hold Supabase JWTs.
- NAT: the Mac can dial out; nothing can dial in.

## Decision

A **relay, not a tunnel**: `rust/relay-ws` (smooai repo) at `relay.smoo.ai`,
cloned from events-ws with per-peer channels.

```
phone ──wss──▶ relay-ws ◀──wss── smooth-daemon (dials OUT)
        Valkey channels relay:{userId}:{deviceId}
        envelopes {to, frame} ⇄ {from, frame} — frames OPAQUE
```

- Every connection is a peer `(JWT sub, ?device=)`. The daemon is device
  `daemon` (v1: one daemon per user); each phone its own id
  (`[A-Za-z0-9._-]{1,64}`, hard-validated — device ids are channel-name
  components).
- The relay forwards `{to, frame}` → `{from, frame}` **same-user-only by
  construction**: the destination channel is keyed on the sender's own
  authenticated userId, so another user's devices are unaddressable. It never
  parses `frame` — the smooth-operator protocol rides through opaquely, so
  protocol evolution never touches the relay.
- Daemon side (`crates/smooth-daemon/src/relay.rs`): a supervisor dials the
  relay with the freshest stored access token (re-read every reconnect — the
  credential heartbeat rotates it), and bridges each phone device to the
  daemon's own operator over a per-device **loopback WS** — the scheduler's
  exact pattern. The operator sees each phone as just another canonical client;
  zero engine changes.
- Knobs: `SMOOTH_RELAY=0` (off), `SMOOTH_RELAY_URL` (endpoint override).
  Signed-out daemons wait and retry; the relay is reachability, never a boot
  dependency. `tailscale serve` stays — direct connection remains first-class
  (and is the debugging path).

## Security posture shift

The tailnet perimeter is gone for relay traffic; what replaces it:

1. **Relay auth**: fail-closed Supabase-JWKS (ES256) on every connection; the
   dev HS256 path is prod-disabled by the SMOODEV-2435 algorithm-confusion
   gate (verbatim from events-ws).
2. **Isolation**: same-user-only forwarding is structural (channel keyed on
   the sender's verified `sub`), tested adversarially (cross-user, channel-
   syntax smuggling, envelope fuzz).
3. **The daemon's own gate stays**: bridged frames still enter through the
   operator's strict-auth loopback WS with the local token, and the permission
   hooks + Narc + kernel sandbox behind it are unchanged.
4. **Blast radius of a stolen Supabase token** is "chat with your own
   daemon" — the same thing the token's owner can do; the relay adds no
   privilege. Frames are TLS'd client↔relay; the relay sees plaintext frames
   (it is Smoo-operated infrastructure, same trust as the LLM gateway).

## Alternatives rejected

- **API Gateway WebSocket (RealtimeApi)**: vestigial post-ADR-026, fails-open
  auth on `$connect`, and Lambda-per-frame pricing/latency for a chatty
  streaming protocol.
- **Extending copilot-ws / chat-ws**: their `/ws` action dispatch belongs to
  the published smooth-operator crate and is not extensible in-repo; hanging a
  relay off the org-chat brain couples two unrelated lifecycles.
- **Tailscale Funnel**: still requires tailscale on the Mac, exposes a public
  URL per machine, and the daemon's docs are emphatic that funnel is never
  enabled.
- **Direct daemon exposure (port forward / dynamic DNS)**: consumer-hostile
  and turns every laptop into an internet-facing origin.

## Consequences

- The Big Smooth apps get one connection story everywhere: direct URL when
  you have LAN/tailnet, relay otherwise — same protocol, one envelope layer.
- Offline daemon = `peer_offline` to the phone (surfaced as "Big Smooth is
  offline"); there is no store-and-forward. Wake-the-Mac is out of scope.
- Stats/REST are not proxied v1 (chat protocol only); the apps hint to
  connect directly for stats until a REST-over-relay follow-up.
- Multi-daemon (two Macs, one user) needs a device-id scheme + picker later;
  v1 pins `daemon`.
