---
'@smooai/smooth': patch
---

`th attest`'s remote target cap is raised from 60,000 to 250,000 `debug/deps` entries (th-86de4d follow-up). Measured on smoo-hub: one cold smooai rust build leaves about 62,000 entries, which was already over the old cap. So every run would have wiped the target and rebuilt cold, taking about 61 minutes. The new cap allows about four builds' worth and still lists in seconds, far below the millions of entries that stalled rustc.
