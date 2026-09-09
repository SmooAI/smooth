---
'@smooai/smooth': minor
---

SmoothFlow relay end-to-end encryption (th-d98fde): a phone pairs with a daemon by scanning a QR (Settings ▸ Phones in the macOS app, or `th flow pair --qr`) carrying the daemon's relay device id, a fresh X25519 public key and a one-time code; both sides derive a pairing key (HKDF-SHA256), the phone proves it with a sealed hello, and from then on every `channel:"flow"` frame between them is ChaCha20-Poly1305 with per-connection session keys and per-direction counter nonces — the Smoo Relay brokers ciphertext only, with no relay change. Pairings persist in `flow.db` (`th flow pair list|revoke`, `/api/flow/pair*`); plaintext from a paired phone is rejected with a visible `flow.error`; Big Smooth chat frames are untouched. The cross-platform vectors live in `crates/smooth-daemon/tests/fixtures/flow-e2e-v1.json`.
