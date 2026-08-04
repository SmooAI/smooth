---
'@smooai/smooth': patch
---

Big Smooth: auto-provision VAPID keys so Web Push works out of the box (pearl th-c561f1).

Notifications used to no-op: the daemon only read VAPID keys from `SMOOTH_VAPID_PUBLIC`/`_PRIVATE`, which are unset on a normal install, so `/push/key` 503'd and "Enable notifications" did nothing. Now the daemon generates a P-256 VAPID keypair on first run (pure-Rust `p256`), persists it to `~/.smooth/vapid.json` (mode 600), and serves it — so push enrolls with no setup. Precedence is unchanged where it matters: explicit `SMOOTH_VAPID_*` env still wins, then the persisted file, then a fresh pair. Windows stays disabled (no sender there). `SMOOTH_VAPID_FILE` overrides the path. Verified the generated key round-trips through `web-push`'s `VapidSignatureBuilder`.
