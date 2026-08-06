---
'@smooai/smooth': patch
---

CI: path-filter PR checks so a non-Rust change stops compiling the workspace twice

`Rust checks` (ubuntu + windows) and `Web checks` ran unconditionally, so a
bash/JSON/doc-only PR burned ~45 min of runner time — Windows bills at 2x — to
validate a shell script. Steps are now gated on `dorny/paths-filter`; the jobs
still run and report so branch protection is unaffected.
