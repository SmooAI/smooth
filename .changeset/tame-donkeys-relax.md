---
'@smooai/smooth': minor
---

Big Smooth registers on the Smoo Relay with a stable per-machine device id instead of the hardcoded `daemon`, so one Smoo account can run several daemons (laptop + smoo-hub) without them fighting over the same relay slot. The id is `daemon-<12 hex>`, minted once and persisted to `~/.smooth/relay-device-id` (mode 600), and the daemon now also announces `label` (the machine's short hostname) and `kind=daemon` so phones can tell the daemons apart. `SMOOTH_RELAY_DEVICE_ID` and `SMOOTH_RELAY_LABEL` override both.
