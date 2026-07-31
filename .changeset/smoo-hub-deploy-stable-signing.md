---
'@smooai/smooth': patch
---

smoo-hub: add `scripts/smoo-hub/deploy.sh` — build + stable code-sign + ship Big Smooth

Deploying `smooth-daemon`/`th` to smoo-hub by copying ad-hoc-signed binaries broke on every push: macOS killed them with `OS_REASON_CODESIGNING`, launchd's recorded LWCR rejected the new cdhash, and any Full Disk Access grant (needed because the workspace is on an external, TCC-gated volume) died with the changing cdhash. `deploy.sh` builds the release binaries locally, signs both with a stable team identity + fixed identifiers (`ai.smoo.smooth-daemon`, `ai.smoo.th`) so the TCC designated requirement is constant across rebuilds, ships them over SSH, and restarts the launchd agent (with a health check and timestamped rollback backups). With a stable DR the FDA grant is granted once and persists; the codesigning/LWCR churn is gone. README documents the one-time keychain "Always Allow" and `th doctor --fix-fda` steps.
