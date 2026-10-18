---
'@smooai/smooth': patch
---

SMOODEV-4018: `th attest` runs a check in the ci-queue light pool when its script has a whole-line `# ci-queue: light` comment, so seconds-long rows no longer wait behind heavy Rust and iOS jobs. Unmarked scripts, and any script that takes the cargo lock, still run heavy.
