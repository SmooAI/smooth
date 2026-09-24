---
'@smooai/smooth': patch
---

`th attest` stops refusing its build box over the wrong disk (th-279151). The remote disk guard checked the host's `/`. On smoo-hub that is a family Mac's system volume, under 1GiB free, while the build lives on `/Volumes/smoo-ext` with terabytes free. So every delegated `rust` attest was refused and quietly re-ran locally on an overloaded laptop: `ci-attest/rust` was credited on 0% of smooai PRs in September. The guard now measures the build volume (`target_dir`, else `worktree`). The check's `TMPDIR` is a fresh `<worktree>.attest-tmp` on that same volume, cleared by the lock holder each run. When the box really is unusable and this machine is above twice its core count, attest no longer starts a near-hour local build whose failures it would distrust anyway: it reports the check as blocked and leaves the row to CI.
