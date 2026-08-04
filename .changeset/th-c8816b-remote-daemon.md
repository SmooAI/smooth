---
'@smooai/smooth': patch
---

Big Smooth desktop: connect to a remote daemon over the tailnet (pearl th-c8816b).

The Electron app could only talk to its own bundled local daemon. Now the tray has a **Connect** submenu — "This Mac (local)" plus any Big Smooth daemons discovered on your Tailscale tailnet (e.g. smoo-hub). Pick one and the window attaches to it.

It works because a remote daemon serves its *own* SPA with its *own* token injected (`web_router_with_token`), so pointing the window at the remote's tailnet URL is a complete, authenticated connection — **nothing is passed by the client**. Discovery shells `tailscale status --json`, enumerates online peers, and probes each on :443 and :8443 for the ungated `/health` (is a daemon here?) and `/api/auth/status` (whose daemon). Switching targets persists the choice (`userData/config.json`) and relaunches; in remote mode the app never spawns or kills a local daemon, and an unreachable remote offers a one-click fall back to This Mac instead of stranding the window.
