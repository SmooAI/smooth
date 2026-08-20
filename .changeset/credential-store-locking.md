---
'@smooai/smooth': patch
---

Stop concurrent `th` processes from revoking each other's session (th-5c0189).

Supabase rotates the refresh token on every exchange and revokes the old one
after a ~10s grace, so two agent sessions that both found an expired session,
both POSTed to Supabase, and both wrote `~/.smooth/auth/*.json` each ended up
holding a token the other had invalidated — one `rename` won and the survivor's
token was not the one live server-side, killing the session until
`th auth login`. `refresh.rs` documented the single-writer rule but enforced it
only by convention.

The whole load → refresh → save sequence now runs under a cross-process advisory
lock (`fs4`, the same primitive `smooth-pearls` locks its registry with), and the
waiter re-reads the file afterwards: whoever queued behind the winner uses the
winner's fresh token instead of minting a second one. `th auth whoami`, `th auth
login`, every `active_org` writer, and `SmoothApiClient::ensure_fresh_token` —
a second M2M refresher that wrote the same file — all go through that lock now
rather than being unlocked second writers.

The lock lives in `smooth-api-client` and is keyed on the credentials *path*,
not on a store type: credentials are written from two crates against two
near-identical store types (one of them in another repo), and a lock only works
if every writer takes the same one.

A refresh that can't be persisted is now an error instead of
`let _ = store.save(...)` printing "✓ session refreshed" and exiting 0 — the
exchange has already revoked the old token at that point, so a silently dropped
write leaves the next run with a dead session.

Secret files are also owner-only from the instant they exist. `fs::write` +
`set_permissions` created them 0644 under the usual umask and only then chmod'ed,
leaving a window another local user could open and hold an fd across — and three
of the five call sites discarded the chmod result, so a failure left the file
0644 permanently and silently. The credentials store, the local operator token,
the VAPID keypair, push subscriptions, the relay device id and `daemon.addr` now
write through a unique `O_EXCL` temp file created 0600 and renamed into place,
with parent directories created 0700.
