---
'smooai-smooth-cli': minor
---

`th service` deploy hardening for self-hosted boxes like smoo-hub (th-c435fd):
- `th service install --env KEY=VALUE` bakes env vars into the LaunchAgent/systemd
  unit, e.g. `--env SMOOTH_ADDR=127.0.0.1:8788 --env SMOOTH_TAILSCALE_HTTPS_PORT=8443`
  to run on a free port + coexist with another tailscale serve on :443.
- `th service self-update [--repo]` pulls latest, runs `pnpm install:th`, and restarts
  the service.
- `th service install-updater [--repo] [--interval]` installs a launchd timer that
  runs self-update on a cadence (default hourly) so the box stays current.
