---
'@smooai/smooth': patch
---

The Smoo Relay link now notices when its socket dies silently (th-6c500f). The relay pings every 30s, but a daemon only ever writes in reply to a ping, so a half-open connection (after sleep or a network change) never errored: Big Smooth sat "connected" for two days while the relay had long since dropped it as a peer and phones saw it offline. The relay supervisor now treats 75s without a single inbound frame as a dead socket, logs it, marks the link `offline`, closes it, and re-dials.
