---
"@smooai/smooth": minor
---

`th service` — background service wrapper for `th up`.

User-level install by default on all three platforms:

- **macOS**: writes `~/Library/LaunchAgents/com.smooai.smooth.plist`,
  drives `launchctl bootstrap gui/$UID`.
- **Linux**: writes `~/.config/systemd/user/smooth.service`, drives
  `systemctl --user enable --now`. Prints a hint to run
  `loginctl enable-linger` so the service survives logout.
- **Windows**: creates a logon-triggered Scheduled Task via `schtasks`.

Commands: `install [--system]`, `uninstall`, `start`, `stop`, `restart`,
`status`, `logs [-f]`. `--system` prints the system-level artifact +
install instructions to stdout instead of running under sudo.

Logs stream to `~/.smooth/service.log` and `~/.smooth/service.err`.
