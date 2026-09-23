---
'@smooai/smooth': minor
---

Big Smooth's window can now drive another of your computers over the Smoo Relay (th-a49e21). A computer switcher at the top of the sidebar lists this computer by name and every other Big Smooth daemon on your relay, for example `smoo-hub`. Picking one reloads the window onto that computer's Big Smooth. Its conversations, `/cd`, Plan/Auto, Stats, `@`-search and safety-judge settings all come from there. The status line and window title name the computer.

Your own daemon does the tunnelling, with the Smoo session it already holds, so there is no extra sign-in. It serves `GET /api/relay/peers` and proxies `/api/relay/peers/<device>/ws` (the operator WebSocket) and `/api/relay/peers/<device>/<route>` (REST, as `"channel":"http"` frames). Each window gets its own relay socket, so two daemons driving each other can't loop.

The proxy only targets the signed-in user's listed daemons. It strips the local token before forwarding. REST is limited to an exact allowlist of the chat surface's routes, and nothing under `/api/flow`, `/api/relay`, `/auth` or `/push` crosses. This computer always works: a remembered computer that is offline, or a relay that is signed out or down, brings the window up locally and says why. See `docs/Architecture/Relay-Computer-Switcher.md`.
