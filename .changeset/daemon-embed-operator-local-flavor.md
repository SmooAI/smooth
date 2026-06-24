---
'smooai-smooth-daemon': minor
'smooai-smooth-cli': patch
---

EPIC th-c89c2a: the daemon can host the OPERATOR's local deployment flavor
in-process. New `smooth_daemon::serve_local_flavor(addr)` boots
smooth-operator's `LocalServer` (lean build — `default-features = false` drops
all cloud adapters: AWS SDK / tokio-postgres / redis / nats; in-memory storage +
backplane), gated by an auto-provisioned local token (`SMOOTH_LOCAL_TOKEN` env →
`~/.smooth/operator-token` mode 600, generated on first run). Because the daemon
*runs the operator*, it speaks the canonical schema-driven WS protocol by
construction — the official widget and the polyglot SDK clients work natively.
Exposed as `th daemon operator [--addr 127.0.0.1:8787]`. Additive: runs
alongside the bespoke `/ws` path while the embed is validated; that bespoke
surface retires once parity lands. (Depends on two local-flavor seams added
upstream in smooth-operator: `LocalTokenVerifier` + `LocalServerBuilder::auth`.)
