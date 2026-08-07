---
'@smooai/smooth': minor
---

Big Smooth dials the Smoo Relay — remote control without tailscale (pearl th-2f626d, EPIC th-5561c5).

The daemon now connects OUT to the Smoo Relay (`wss://relay.smoo.ai/ws`, smooai SMOODEV-2828) as the signed-in user's device `daemon`, authenticating with the stored Smoo session (re-read fresh on every reconnect — the credential heartbeat rotates it). Phones connect to the same relay as their own devices; the relay forwards opaque `{to, frame}` envelopes same-user-only, and the daemon bridges each phone to its own operator over a per-device loopback WS (`ws://127.0.0.1:<port>/ws?token=…` — the scheduler's exact seam), so the operator sees each phone as just another canonical-protocol client. Exponential-backoff reconnect; a signed-out daemon waits quietly; `SMOOTH_RELAY=0` disables; `SMOOTH_RELAY_URL` overrides the endpoint. This is the foundation for the Big Smooth iOS/Android apps (SMOODEV-2829/2830).
