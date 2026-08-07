---
'@smooai/smooth': minor
---

Big Smooth notifications actually fire (pearl th-b9a636, EPIC th-5561c5): scheduled/proactive turns now notify the user on completion.

`push.rs` could *send* web pushes since th-c561f1 but nothing ever triggered one — a reminder ran its turn and the result sat silently in the conversation. New `notify.rs` (`TurnNotifier`) fans out on scheduled-turn completion: (1) the existing VAPID **web push** to the installed PWA (`PushState` now escapes `push_router()` so the scheduler can hold it), and (2) **phone push** via the Smoo platform's existing `POST /organizations/{org}/notifications/self` (in-app + FCM to the user's devices, deep link `bigsmooth://chat` for the Big Smooth mobile apps) using the daemon's stored Smoo session — no push credentials on the daemon, no new backend surface. The scheduler's turn driver captures the final response text and uses it as the notification body. Signed-out or unreachable channels skip quietly; notifications never fail a schedule.
