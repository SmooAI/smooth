---
"smooai-smooth-daemon": minor
"smooai-smooth-web": minor
---

Web Push: Big Smooth can notify your phone (an installed PWA) with the tab closed.
Daemon serves `/push/key` (VAPID public key), `/push/subscribe` (persists to
~/.smooth/push-subs.json), and `/push/test`; `PushState::send_to_all` VAPID-signs +
encrypts via the `web-push` crate and prunes expired endpoints. VAPID keys come from
`SMOOTH_VAPID_PUBLIC`/`SMOOTH_VAPID_PRIVATE` (unset ⇒ routes 503, push off). Frontend
adds a "Notify me" bell (top-right), a `usePush` enrollment hook, and a `push-sw.js`
service-worker handler (imported into the generated SW) that shows the notification and
focuses the app on tap.
