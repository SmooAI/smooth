---
'@smooai/smooth': minor
---

th-e82dac (SMOODEV-401): `th tunnel` client is live — dials the th.smoo.ai rendezvous over WebSocket, does the ClientHello/ServerHello handshake, prints the assigned `<slug>.th.smoo.ai` public URL, and multiplexes inbound HTTP requests back to the local Big Smooth using the house `smooai-fetch` client (retries off, per-request timeout + circuit breaker). WebSocket-stream proxying and binary-response fidelity are the remaining follow-ups. `TunnelClient::run` now takes an `on_ready` callback and returns real transport/service errors instead of `NotImplementedYet`.
