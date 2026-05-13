---
'@smooai/smooth': patch
---

Make `host_tool` actually reach Big Smooth from inside a microsandbox
VM on the 0.3.14 version we're pinned at. `host.containers.internal`
has no DNS entry on 0.3.14 (that's a 0.4+ feature), and `127.0.0.1`
from inside the guest routes via the guest's own loopback — never
reaching the host-side TCP proxy. SMOOTH_NARC_URL now uses a
routable host IP detected via the UDP-connect-to-public-IP-and-read-
local-addr trick. Big Smooth listens on `0.0.0.0:4400`, so any of
the host's real interface IPs lands on the listener; microsandbox's
proxy `TcpStream::connect()`s the destination IP as-is. RFC1918
addresses pass `NetworkPolicy::allow_all()` (which
`allow_host_loopback: true` already enables). `SMOOTH_NARC_URL`
env override still wins.
