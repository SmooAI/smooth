---
'@smooai/smooth': patch
---

th doctor: detect a workspace on a TCC-gated external volume + guided Full Disk Access helper

Big Smooth's workspace can live on an external volume (on smoo-hub `~/dev` is a symlink to `/Volumes/smoo-ext`). macOS gates external volumes behind TCC (`kTCCServiceSystemPolicyRemovableVolumes`), so a daemon/`th` without the grant gets `EPERM` on every filesystem op there and looks jailed in its own workspace — separate from the seatbelt sandbox. `th doctor` now flags when the resolved workspace is on a non-boot `/Volumes` path (and whether access is already denied), and `th doctor --fix-fda` opens the Full Disk Access settings pane and reveals `th` + the daemon binary in Finder to drag in. FDA can't be granted programmatically (SIP-protected TCC.db), so this is as automated as the grant gets; it also warns that an ad-hoc-signed `th` loses the grant on every rebuild.
